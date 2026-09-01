//! The rules. Every one of them is a pure function over answers already
//! collected, so the policy can be tested without a network — and reviewed
//! without reading an HTTP client.
//!
//! The rules are asymmetric on purpose, because the failure modes are. An
//! unnoticed dropped broadcast or a wrong height can cost a user their money,
//! while an extra HTTP request costs nothing.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::error::Error;
use alloc::collections::BTreeMap;

use crate::types::{AddressTx, Answer, Outspend, TxInfo, TxStatus};

/// A tip more than this many blocks above the median is treated as a lying or
/// broken provider and ignored. Six blocks is far beyond honest propagation
/// skew, so this only excludes nonsense.
pub const MAX_TIP_SKEW: u64 = 6;

/// Below this, "drop the outlier and trust the majority" is not available: two
/// providers can only agree or disagree, never out-vote each other.
pub const MIN_PROVIDERS_FOR_MAJORITY: usize = 3;

/// A warning when there are too few providers for the outlier filter to do
/// anything.
///
/// [`MAX_TIP_SKEW`] drops heights above the median, and with two providers the
/// median IS the higher of the two — so nothing is ever filtered and a single
/// provider reporting a wrong tip decides when a timelock has elapsed. A
/// deployment should learn that at startup, not from a lost refund.
pub fn quorum_warning(providers: usize) -> Option<String> {
  (providers < MIN_PROVIDERS_FOR_MAJORITY).then(|| {
    format!(
      "only {providers} esplora provider(s) configured; the tip outlier filter needs \
       {MIN_PROVIDERS_FOR_MAJORITY} to out-vote a liar and is a no-op below that"
    )
  })
}

/// Turn per-provider results into the answers to reconcile.
///
/// An error only when NOT ONE provider answered, since "all down" and "one
/// down" call for different responses. A 404 rides through as `(base, None)`
/// because it is an answer — the chain saying no — not a failure to ask.
///
/// Returns the answers and the failures, so a caller can log the partial
/// outage without this crate choosing how.
#[allow(clippy::type_complexity)]
pub fn collect<T, E: core::fmt::Display>(
  what: &str,
  results: impl IntoIterator<Item = (String, Result<Option<T>, E>)>,
) -> Result<(Vec<Answer<T>>, Vec<String>), Error> {
  let mut answers = Vec::new();
  let mut failures = Vec::new();
  for (base, res) in results {
    match res {
      Ok(v) => answers.push((base, v)),
      Err(e) => failures.push(format!("{base}: {e}")),
    }
  }
  if answers.is_empty() {
    return Err(Error::NoProviderAnswered {
      what: what.to_string(),
      failures: failures.join("; "),
    });
  }
  Ok((answers, failures))
}

/// The tip the providers agree on: drop anything more than [`MAX_TIP_SKEW`]
/// above the median, then take the highest of what is left.
///
/// The fold leans high on purpose. Being early costs a rejected retry; being
/// late costs a refund its race.
///
/// `None` only when nobody answered.
pub fn agreed_tip(mut heights: Vec<u64>) -> Option<u64> {
  if heights.is_empty() {
    return None;
  }
  heights.sort_unstable();
  let median = heights[heights.len() / 2];
  // The median always survives its own filter, so `max` is never `None` here.
  heights
    .into_iter()
    .filter(|h| *h <= median + MAX_TIP_SKEW)
    .max()
}

/// Reconcile per-provider `/tx/:txid` answers.
///
/// Asymmetric on purpose:
///
/// - "never heard of it", "in the mempool" and "confirmed" are NOT a
///   disagreement — that is ordinary propagation lag, and treating it as an
///   error would flap red every time something is broadcast. The most advanced
///   answer wins, matching the tip policy.
/// - two providers reporting the transaction confirmed at DIFFERENT heights is
///   irreconcilable — one is lying or on a fork — and fails loudly rather than
///   picking a winner. Only two KNOWN heights conflict: an answer that claims
///   confirmation without naming a block contradicts nothing, and is treated as
///   the less informative of the two rather than as a fork.
pub fn reconcile_tx(txid: &str, answers: Vec<Answer<TxInfo>>) -> Result<Option<TxInfo>, Error> {
  let mut best: Option<(String, TxInfo)> = None;
  for (base, info) in answers {
    let Some(info) = info else { continue };
    if let Some((prev_base, prev)) = &best
      && let Some(h) = info.status.block_height
      && let Some(prev_h) = prev.status.block_height
      && info.status.confirmed
      && prev.status.confirmed
      && prev_h != h
    {
      return Err(Error::HeightDisagreement {
        txid: txid.to_string(),
        a: prev_base.clone(),
        a_height: prev_h,
        b: base,
        b_height: h,
      });
    }
    match (&best, info.status.block_height) {
      // Keep the answer that is furthest along. A heightless answer never
      // displaces a confirmed one, whether it is honestly unconfirmed or
      // claims confirmation without saying where.
      (Some((_, prev)), None) if prev.status.confirmed => {}
      (Some((_, prev)), _) if prev.status.confirmed && !info.status.confirmed => {}
      _ => best = Some((base, info)),
    }
  }
  Ok(best.map(|(_, info)| info))
}

/// Reconcile per-provider `/tx/:txid/hex` answers.
///
/// One provider having it and another 404ing is propagation lag, not a
/// disagreement: the one that has it wins. A transaction's serialization is
/// determined by its txid, so two providers returning different bytes for the
/// same txid cannot both be right.
pub fn reconcile_hex(txid: &str, answers: Vec<Answer<String>>) -> Result<Option<String>, Error> {
  let mut found: Option<(String, String)> = None;
  for (base, hex) in answers {
    let Some(hex) = hex else { continue };
    let hex = hex.trim().to_string();
    match &found {
      Some((prev_base, prev)) if *prev != hex => {
        return Err(Error::BytesDisagreement {
          txid: txid.to_string(),
          a: prev_base.clone(),
          b: base,
        });
      }
      _ => found = Some((base, hex)),
    }
  }
  Ok(found.map(|(_, hex)| hex))
}

/// The outcome of reconciling outspend answers, and whether it was contested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spend {
  pub outspend: Outspend,
  /// The provider that reported the output UNSPENT while another reported it
  /// spent — the one whose answer was overridden, named so a caller can say
  /// which provider is behind.
  ///
  /// Not an error: a mempool spend has simply not reached everyone. Worth
  /// surfacing all the same, since believing the spend is a deliberate choice.
  /// Always `None` when the reported spend was uncontested.
  pub contested_by: Option<String>,
}

/// Reconcile per-provider outspend answers.
///
/// A hidden spend is the dangerous direction — it is how a punishing
/// transaction stays invisible while a watcher keeps re-broadcasting a refund
/// that can no longer win — so ANY provider reporting the output spent is
/// believed over one reporting it unspent. Two providers naming DIFFERENT
/// spenders cannot both be right, and that fails loudly.
///
/// When they name the SAME spender, the most advanced answer about it wins —
/// as in [`reconcile_tx`] — so a caller asking how deep the spend is gets the
/// confirmed status if any provider has it, rather than whichever answer
/// happened to be listed last.
/// How much an answer says about a spend: nothing, in the mempool, confirmed.
fn spend_depth(o: &Outspend) -> u8 {
  match &o.status {
    Some(s) if s.confirmed => 2,
    Some(_) => 1,
    None => 0,
  }
}

pub fn reconcile_outspend(
  txid: &str,
  vout: u32,
  answers: Vec<Answer<Outspend>>,
) -> Result<Spend, Error> {
  let mut unspent: Option<(String, Outspend)> = None;
  let mut spent: Option<(String, Outspend)> = None;
  for (base, o) in answers {
    let Some(o) = o else { continue };
    if !o.spent {
      unspent.get_or_insert((base, o));
      continue;
    }
    match &spent {
      Some((prev_base, prev)) if prev.txid != o.txid => {
        return Err(Error::SpenderDisagreement {
          outpoint: format!("{txid}:{vout}"),
          a: prev_base.clone(),
          a_txid: prev.txid.clone(),
          b: base,
          b_txid: o.txid.clone(),
        });
      }
      // Same spender: keep whichever answer says the most about it.
      _ => {
        if spent
          .as_ref()
          .is_none_or(|(_, prev)| spend_depth(&o) > spend_depth(prev))
        {
          spent = Some((base, o));
        }
      }
    }
  }

  if let Some((_, outspend)) = spent {
    return Ok(Spend {
      outspend,
      contested_by: unspent.map(|(base, _)| base),
    });
  }
  unspent
    .map(|(_, outspend)| Spend {
      outspend,
      contested_by: None,
    })
    .ok_or_else(|| Error::NotFound {
      what: format!("outspend {txid}:{vout}"),
    })
}

/// Reconcile `/address/:addr/txs` across providers.
///
/// # Two rules, because the two mistakes do not cost the same
///
/// A watcher built on this tells somebody whether their money arrived. Saying
/// "confirmed" when it is not is worse than saying "not yet" when it is: the
/// first ends the user's attention, the second only prolongs it. So existence
/// and confirmation are reconciled differently.
///
/// **Existence is a union.** Any provider reporting a transaction that pays the
/// address is enough for it to appear here. A provider that has not seen a
/// mempool transaction yet is behind, not authoritative, and waiting for
/// agreement would mean the slowest provider setting the pace for every deposit.
///
/// **Confirmation is a majority.** A transaction is returned confirmed only if
/// more than half of the providers that ANSWERED say it is. Otherwise it is
/// downgraded to seen-but-unconfirmed: the deposit still shows up, it just does
/// not yet carry the claim that ends the user's attention.
///
/// With a single provider a majority is whatever it says -- which is the old
/// single-provider behaviour, reached honestly. [`quorum_warning`] is what tells
/// an operator they are in that position.
///
/// # What is an error rather than a downgrade
///
/// Two confirmed answers naming different block heights, or any two answers
/// naming different values for the same output. Neither is lag: a transaction's
/// height and its outputs are fixed once it is mined. One provider is wrong
/// about money, and the caller must not pick a side.
pub fn reconcile_address_txs(
  address: &str,
  answers: Vec<Answer<Vec<AddressTx>>>,
) -> Result<Vec<AddressTx>, Error> {
  // Only providers that actually answered get a vote; an unreachable one must
  // not count towards the majority it never took part in.
  let mut answered = 0usize;
  let mut by_txid: BTreeMap<String, Vec<(String, AddressTx)>> = BTreeMap::new();

  for (base, txs) in answers {
    let Some(txs) = txs else { continue };
    answered += 1;
    for tx in txs {
      if tx.output_to(address).is_none() {
        continue;
      }
      by_txid
        .entry(tx.txid.clone())
        .or_default()
        .push((base.clone(), tx));
    }
  }

  let mut out = Vec::with_capacity(by_txid.len());
  for (txid, copies) in by_txid {
    let mut confirmed_votes = 0usize;
    let mut height: Option<(String, u64)> = None;
    let mut value: Option<(String, u64)> = None;

    for (base, tx) in &copies {
      if tx.status.confirmed {
        confirmed_votes += 1;
      }
      if let (Some(h), true) = (tx.status.block_height, tx.status.confirmed) {
        match &height {
          Some((prev_base, prev_h)) if *prev_h != h => {
            return Err(Error::HeightDisagreement {
              txid: txid.clone(),
              a: prev_base.clone(),
              a_height: *prev_h,
              b: base.clone(),
              b_height: h,
            });
          }
          None => height = Some((base.clone(), h)),
          _ => {}
        }
      }
      if let Some((_, out)) = tx.output_to(address) {
        match &value {
          Some((prev_base, prev_v)) if *prev_v != out.value => {
            return Err(Error::ValueDisagreement {
              txid: txid.clone(),
              address: address.to_string(),
              a: prev_base.clone(),
              a_value: *prev_v,
              b: base.clone(),
              b_value: out.value,
            });
          }
          None => value = Some((base.clone(), out.value)),
          _ => {}
        }
      }
    }

    let majority_confirmed = confirmed_votes * 2 > answered;

    // Prefer a confirmed copy when the majority agrees -- it carries the height
    // and block time. Otherwise take any copy and strip the claim.
    let mut chosen = copies
      .iter()
      .find(|(_, tx)| tx.status.confirmed == majority_confirmed)
      .or_else(|| copies.first())
      .map(|(_, tx)| tx.clone())
      .expect("a txid exists because at least one copy produced it");

    if !majority_confirmed {
      chosen.status = TxStatus {
        confirmed: false,
        block_height: None,
        block_hash: None,
        block_time: None,
      };
    }
    out.push(chosen);
  }

  Ok(out)
}

/// Does this rejection mean "we already have it"?
///
/// Matched against the KNOWN wordings, not the bare word "already": a bare
/// substring match swallows real rejections that merely mention it, and a
/// caller uses this to decide whether a failed broadcast is routine or an
/// emergency.
pub fn is_already_known(msg: &str) -> bool {
  let m = msg.to_ascii_lowercase();
  [
    "txn-already-in-mempool",
    "txn-already-known",
    "transaction already in block chain",
    "transaction already in mempool",
    // bitcoind's RPC wrapper around the two mempool rejects above.
    "already in mempool",
    "already have transaction",
    // bitcoind >= 25 for an already-mined transaction.
    "transaction outputs already in utxo set",
  ]
  .iter()
  .any(|known| m.contains(known))
}

#[cfg(test)]
mod address_tests {
  use super::*;
  use alloc::vec;

  const ADDR: &str = "bc1qexample";

  fn tx(txid: &str, confirmed: bool, height: Option<u64>, value: u64) -> AddressTx {
    AddressTx {
      txid: txid.to_string(),
      status: TxStatus {
        confirmed,
        block_height: height,
        block_hash: None,
        block_time: height.map(|h| 1_700_000_000 + h),
      },
      vout: vec![crate::types::Vout {
        scriptpubkey_address: Some(ADDR.to_string()),
        value,
      }],
    }
  }

  fn from(base: &str, txs: Vec<AddressTx>) -> Answer<Vec<AddressTx>> {
    (base.to_string(), Some(txs))
  }

  /// Existence unions: one provider seeing a deposit is enough to report it.
  ///
  /// The alternative is the slowest provider setting the pace for every
  /// deposit, and a mempool transaction one provider has not indexed yet is
  /// evidence of lag, not of absence.
  #[test]
  fn a_single_provider_seeing_a_deposit_is_enough_to_report_it() {
    let got = reconcile_address_txs(
      ADDR,
      vec![
        from("a", vec![tx("aa", false, None, 50_000)]),
        from("b", vec![]),
        from("c", vec![]),
      ],
    )
    .unwrap();
    assert_eq!(got.len(), 1, "the deposit should still be reported");
    assert!(!got[0].status.confirmed);
  }

  /// Confirmation needs a majority, because "confirmed" is the claim that ends
  /// the user's attention.
  #[test]
  fn one_provider_alone_cannot_confirm() {
    let got = reconcile_address_txs(
      ADDR,
      vec![
        from("a", vec![tx("aa", true, Some(800_000), 50_000)]),
        from("b", vec![tx("aa", false, None, 50_000)]),
        from("c", vec![tx("aa", false, None, 50_000)]),
      ],
    )
    .unwrap();
    assert_eq!(got.len(), 1);
    assert!(
      !got[0].status.confirmed,
      "one vote out of three is not a majority"
    );
    assert_eq!(
      got[0].status.block_height, None,
      "a downgrade drops the height too"
    );
  }

  #[test]
  fn two_of_three_confirms() {
    let got = reconcile_address_txs(
      ADDR,
      vec![
        from("a", vec![tx("aa", true, Some(800_000), 50_000)]),
        from("b", vec![tx("aa", true, Some(800_000), 50_000)]),
        from("c", vec![tx("aa", false, None, 50_000)]),
      ],
    )
    .unwrap();
    assert!(got[0].status.confirmed);
    assert_eq!(got[0].status.block_height, Some(800_000));
  }

  /// A provider that never answered cannot count towards the majority it took
  /// no part in -- otherwise an outage silently raises the bar for confirming.
  #[test]
  fn silence_is_not_a_vote_against() {
    let got = reconcile_address_txs(
      ADDR,
      vec![
        from("a", vec![tx("aa", true, Some(800_000), 50_000)]),
        ("b".to_string(), None),
        ("c".to_string(), None),
      ],
    )
    .unwrap();
    assert!(
      got[0].status.confirmed,
      "one of one answering is a majority"
    );
  }

  /// Not lag: a mined transaction's height is fixed, so two confirmed answers
  /// naming different ones means somebody is wrong.
  #[test]
  fn confirmed_answers_at_different_heights_are_an_error() {
    let err = reconcile_address_txs(
      ADDR,
      vec![
        from("a", vec![tx("aa", true, Some(800_000), 50_000)]),
        from("b", vec![tx("aa", true, Some(800_001), 50_000)]),
      ],
    )
    .unwrap_err();
    assert!(matches!(err, Error::HeightDisagreement { .. }));
  }

  /// The one that costs money directly. Outputs are fixed by the txid, so a
  /// disagreement about value is a provider lying or corrupt -- and a deposit
  /// watcher deciding whether ENOUGH arrived must not pick a side.
  #[test]
  fn disagreeing_about_what_was_paid_is_an_error() {
    let err = reconcile_address_txs(
      ADDR,
      vec![
        from("a", vec![tx("aa", true, Some(800_000), 50_000)]),
        from("b", vec![tx("aa", true, Some(800_000), 90_000)]),
      ],
    )
    .unwrap_err();
    assert!(matches!(err, Error::ValueDisagreement { .. }));
  }

  /// Transactions that do not pay the watched address are dropped: Esplora
  /// returns everything TOUCHING an address, including the spends of it.
  #[test]
  fn transactions_that_do_not_pay_the_address_are_ignored() {
    let mut spend = tx("bb", true, Some(800_000), 50_000);
    spend.vout[0].scriptpubkey_address = Some("bc1qsomewhereelse".to_string());
    let got = reconcile_address_txs(ADDR, vec![from("a", vec![spend])]).unwrap();
    assert!(got.is_empty());
  }
}
