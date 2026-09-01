//! A ready-made async client over `reqwest`, for callers that want one.
//!
//! Everything here is fetching and fan-out; every decision lives in
//! [`crate::policy`]. That split is the point of the crate — a browser can
//! ignore this module entirely and still get the same verdicts.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use futures_util::future::join_all;

use crate::error::Error;
use crate::policy;
use crate::types::{AddressTx, TxInfo, normalise, route};

/// Multi-provider Esplora client.
#[derive(Clone)]
pub struct Chain {
  bases: Vec<String>,
  http: reqwest::Client,
}

impl Chain {
  /// `bases` are API roots, e.g. `https://blockstream.info/api`. Trailing
  /// slashes are trimmed and blanks dropped.
  ///
  /// Deliberately does NOT substitute defaults for an empty list. Which
  /// providers to trust is the caller's decision, and silently supplying our
  /// favourites would make a misconfigured deployment look configured.
  pub fn new(bases: Vec<String>) -> Result<Self, Error> {
    let bases = normalise(bases);
    if bases.is_empty() {
      return Err(Error::NoProviders);
    }
    let http = reqwest::Client::builder()
      .timeout(core::time::Duration::from_secs(30))
      .build()
      .map_err(|e| Error::Http(e.to_string()))?;
    Ok(Self { bases, http })
  }

  /// The configured provider roots.
  pub fn providers(&self) -> &[String] {
    &self.bases
  }

  /// See [`policy::quorum_warning`].
  pub fn quorum_warning(&self) -> Option<String> {
    policy::quorum_warning(self.bases.len())
  }

  /// Chain tip, asked of every provider — never first-answer, because a wrong
  /// height decides when a timelock has elapsed.
  pub async fn tip_height(&self) -> Result<u64, Error> {
    let path = route::tip_height();
    let gets = self.bases.iter().map(|base| async move {
      let out = self
        .text(&format!("{base}{path}"))
        .await
        .and_then(|body| match body {
          Some(t) => t
            .trim()
            .parse::<u64>()
            .map(Some)
            .map_err(|e| Error::Http(format!("unparseable height {:?}: {e}", t.trim()))),
          None => Ok(None),
        });
      (base.clone(), out)
    });
    let (answers, _failures) = policy::collect(path, join_all(gets).await)?;
    let heights: Vec<u64> = answers.into_iter().filter_map(|(_, h)| h).collect();
    policy::agreed_tip(heights).ok_or_else(|| Error::NoProviderAnswered {
      what: path.to_string(),
      failures: "every provider answered without a height".into(),
    })
  }

  /// `None` if NO provider has heard of the transaction.
  pub async fn tx(&self, txid: &str) -> Result<Option<TxInfo>, Error> {
    let path = route::tx(txid);
    let (answers, _) = policy::collect(&path, self.json_all(&path).await)?;
    policy::reconcile_tx(txid, answers)
  }

  /// Raw transaction hex, or `None` if NO provider has heard of it.
  /// Every transaction paying `address`, reconciled across providers.
  ///
  /// Existence unions and confirmation takes a majority -- see
  /// [`policy::reconcile_address_txs`] for why those two differ.
  pub async fn address_txs(&self, address: &str) -> Result<Vec<AddressTx>, Error> {
    let path = route::address_txs(address);
    let (answers, _) = policy::collect(&path, self.json_all(&path).await)?;
    policy::reconcile_address_txs(address, answers)
  }

  pub async fn tx_hex(&self, txid: &str) -> Result<Option<String>, Error> {
    let path = route::tx_hex(txid);
    let gets = self.bases.iter().map(|base| {
      let url = format!("{base}{path}");
      async move { (base.clone(), self.text(&url).await) }
    });
    let (answers, _) = policy::collect(&path, join_all(gets).await)?;
    policy::reconcile_hex(txid, answers)
  }

  /// What spent `txid:vout`, confirmed or in the mempool.
  pub async fn outspend(&self, txid: &str, vout: u32) -> Result<policy::Spend, Error> {
    let path = route::outspend(txid, vout);
    let (answers, _) = policy::collect(&path, self.json_all(&path).await)?;
    policy::reconcile_outspend(txid, vout, answers)
  }

  /// Confirmations for `txid`: 0 when in the mempool, `None` when unknown.
  pub async fn confirmations(&self, txid: &str, tip: u64) -> Result<Option<u64>, Error> {
    Ok(self.tx(txid).await?.map(|i| crate::confirmations(&i, tip)))
  }

  /// Broadcast to EVERY provider in parallel.
  ///
  /// One provider silently dropping a transaction must not lose a race, and
  /// "dropped" cannot be told from "accepted" after the fact. Sending twice is
  /// free; sending once and being wrong is not. A rejection meaning "we already
  /// have it" counts as acceptance.
  ///
  /// Returns the txid a provider echoed back. Empty when the only acceptance
  /// was an acknowledgement carrying no id — the transaction is in, and the
  /// caller already knows its txid, having built it.
  pub async fn broadcast(&self, tx_hex: &str) -> Result<String, Error> {
    let path = route::broadcast();
    let sends = self.bases.iter().map(|base| {
      let url = format!("{base}{path}");
      async move {
        let res = self.http.post(&url).body(tx_hex.to_string()).send().await;
        (base.clone(), res)
      }
    });

    // An echoed txid and a bare "we already have it" are both acceptance, but
    // only one of them carries an id. Tracked apart so that whichever provider
    // answers first, a real txid is never displaced by the acknowledgement.
    let mut txid: Option<String> = None;
    let mut already_known = false;
    let mut failures = Vec::new();
    for (base, res) in join_all(sends).await {
      match res {
        Ok(r) => {
          let ok = r.status().is_success();
          let body = r.text().await.unwrap_or_default();
          let body = body.trim();
          if ok {
            if body.is_empty() {
              already_known = true;
            } else if txid.is_none() {
              txid = Some(body.to_string());
            }
          } else if policy::is_already_known(body) {
            already_known = true;
          } else {
            failures.push(format!("{base}: {body}"));
          }
        }
        Err(e) => failures.push(format!("{base}: {e}")),
      }
    }
    match txid {
      Some(id) => Ok(id),
      None if already_known => Ok(String::new()),
      None => Err(Error::BroadcastRejected {
        failures: failures.join("; "),
      }),
    }
  }

  /// GET returning `None` on 404 — an answer, not a failure.
  async fn text(&self, url: &str) -> Result<Option<String>, Error> {
    let r = self
      .http
      .get(url)
      .send()
      .await
      .map_err(|e| Error::Http(e.to_string()))?;
    if r.status() == reqwest::StatusCode::NOT_FOUND {
      return Ok(None);
    }
    if !r.status().is_success() {
      return Err(Error::Http(format!("HTTP {}", r.status())));
    }
    r.text()
      .await
      .map(Some)
      .map_err(|e| Error::Http(e.to_string()))
  }

  async fn json_all<T: serde::de::DeserializeOwned>(
    &self,
    path: &str,
  ) -> Vec<(String, Result<Option<T>, Error>)> {
    let gets = self.bases.iter().map(|base| async move {
      let out = match self.text(&format!("{base}{path}")).await {
        Ok(Some(body)) => serde_json::from_str::<T>(&body)
          .map(Some)
          .map_err(|e| Error::Http(format!("decoding {path}: {e}"))),
        Ok(None) => Ok(None),
        Err(e) => Err(e),
      };
      (base.clone(), out)
    });
    join_all(gets).await
  }
}

impl core::fmt::Debug for Chain {
  fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
    f.debug_struct("Chain")
      .field("providers", &self.bases)
      .finish()
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn providers_are_normalised_and_never_defaulted() {
    let c = Chain::new(alloc::vec![
      "https://a.example/api/".into(),
      "  ".into(),
      "https://b.example/api".into(),
    ])
    .unwrap();
    assert_eq!(
      c.providers(),
      ["https://a.example/api", "https://b.example/api"]
    );
    assert!(matches!(Chain::new(alloc::vec![]), Err(Error::NoProviders)));
  }

  #[test]
  fn too_few_providers_is_warned_about() {
    let c = Chain::new(alloc::vec!["https://a.example/api".into()]).unwrap();
    assert!(c.quorum_warning().is_some());
  }
}
