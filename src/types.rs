//! The Esplora REST shapes this crate reads, and the answer type it reconciles.

use alloc::string::String;
use alloc::vec::Vec;

/// Where a transaction sits, straight from the provider.
///
/// `block_height` is reported directly rather than derived from
/// tip-minus-confirmations, so a disagreement between providers about the tip
/// cannot corrupt it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct TxStatus {
  pub confirmed: bool,
  pub block_height: Option<u64>,
  pub block_hash: Option<String>,
  pub block_time: Option<u64>,
}

/// A transaction as `/tx/:txid` reports it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct TxInfo {
  pub txid: String,
  pub status: TxStatus,
}

/// What spent an output, if anything.
///
/// Covers mempool spenders as well as confirmed ones, which is what makes a
/// mempool-and-block scan unnecessary: one request answers "has this been
/// spent, and by what".
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Outspend {
  pub spent: bool,
  /// Spending txid. Present iff `spent`.
  pub txid: Option<String>,
  pub status: Option<TxStatus>,
}

/// One output of a transaction, as `/address/:addr/txs` reports it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct Vout {
  /// The address this output pays, when Esplora can decode one.
  pub scriptpubkey_address: Option<String>,
  /// Value in satoshis.
  pub value: u64,
}

/// A transaction touching a watched address.
///
/// Only the fields a watcher reads: which transaction, whether it has landed,
/// and what it paid. Esplora returns considerably more, and deserialising the
/// rest would tie this crate to fields it has no opinion about.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct AddressTx {
  pub txid: String,
  pub status: TxStatus,
  pub vout: Vec<Vout>,
}

impl AddressTx {
  /// The first output paying `address`, with its index.
  ///
  /// First, not the sum: a deposit is one output, and a transaction that pays
  /// the same address twice is not something a watcher should quietly total up.
  pub fn output_to(&self, address: &str) -> Option<(u32, &Vout)> {
    self
      .vout
      .iter()
      .enumerate()
      .find(|(_, o)| o.scriptpubkey_address.as_deref() == Some(address))
      .map(|(i, o)| (i as u32, o))
  }
}

/// One provider's answer to one question.
///
/// `None` is an answer — a 404, the chain saying it has never heard of the
/// thing — and is deliberately distinct from a provider that could not be
/// reached, which never becomes an `Answer` at all. Conflating the two is how a
/// lagging provider turns into "the transaction does not exist".
pub type Answer<T> = (String, Option<T>);

/// Confirmations for a transaction against an agreed tip: 0 while it is in the
/// mempool.
///
/// Uses the provider's own `block_height`, so a provider lying about the tip
/// cannot inflate the depth of a transaction it reported honestly.
pub fn confirmations(info: &TxInfo, tip: u64) -> u64 {
  match info.status.block_height {
    Some(h) if info.status.confirmed => tip.saturating_sub(h) + 1,
    _ => 0,
  }
}

/// The routes this crate reads, as paths to append to a provider's API root.
pub mod route {
  use alloc::format;
  use alloc::string::String;

  pub fn tip_height() -> &'static str {
    "/blocks/tip/height"
  }
  pub fn tx(txid: &str) -> String {
    format!("/tx/{txid}")
  }
  pub fn tx_hex(txid: &str) -> String {
    format!("/tx/{txid}/hex")
  }
  pub fn outspend(txid: &str, vout: u32) -> String {
    format!("/tx/{txid}/outspend/{vout}")
  }
  pub fn broadcast() -> &'static str {
    "/tx"
  }
  pub fn address_txs(address: &str) -> String {
    format!("/address/{address}/txs")
  }
}

/// Normalise provider roots: surrounding whitespace and trailing slashes
/// removed, blanks dropped, duplicates dropped, order kept.
///
/// Whitespace is trimmed before the blank test, so an all-space entry — the
/// shape a stray comma in a config list produces — is dropped rather than
/// becoming a provider whose every URL starts with a space.
///
/// Duplicates go because the count is load-bearing: one host listed three
/// times is one provider, and leaving it as three would silence
/// [`crate::policy::quorum_warning`] on a deployment that has no quorum at all.
pub fn normalise(bases: impl IntoIterator<Item = String>) -> Vec<String> {
  let mut out: Vec<String> = Vec::new();
  for base in bases {
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() || out.iter().any(|kept| kept == base) {
      continue;
    }
    out.push(base.into());
  }
  out
}
