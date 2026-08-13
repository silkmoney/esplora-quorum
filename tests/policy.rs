//! The reconciliation rules, tested without a network.
//!
//! These came with the code from the deployment they were written for, where
//! each one was a real failure mode rather than a hypothetical: a hidden spend
//! that let a punishing transaction stay invisible, a lagging provider that
//! collapsed a lookup to "never heard of it", a wrong tip that decided a
//! timelock had elapsed.

use esplora_quorum::policy::{
  self, MAX_TIP_SKEW, agreed_tip, is_already_known, reconcile_hex, reconcile_outspend, reconcile_tx,
};
use esplora_quorum::{Error, Outspend, TxInfo, TxStatus};

fn status(confirmed: bool, height: Option<u64>) -> TxStatus {
  TxStatus {
    confirmed,
    block_height: height,
    block_hash: None,
    block_time: None,
  }
}

fn info(confirmed: bool, height: Option<u64>) -> TxInfo {
  TxInfo {
    txid: "t".into(),
    status: status(confirmed, height),
  }
}

fn spent_by(txid: &str) -> Outspend {
  Outspend {
    spent: true,
    txid: Some(txid.into()),
    status: Some(status(true, Some(100))),
  }
}

fn unspent() -> Outspend {
  Outspend {
    spent: false,
    txid: None,
    status: None,
  }
}

// ── outspend ────────────────────────────────────────────────────────────────

#[test]
fn outspend_bails_when_providers_name_different_spenders() {
  // Two providers cannot both be right about what spent an output. Picking one
  // would mean guessing whether the spender was a refund or a punish.
  let err = reconcile_outspend(
    "lock",
    0,
    vec![
      ("A".into(), Some(spent_by("aa"))),
      ("B".into(), Some(spent_by("bb"))),
    ],
  )
  .unwrap_err();
  assert!(matches!(err, Error::SpenderDisagreement { .. }), "{err}");
}

#[test]
fn outspend_believes_spent_over_unspent() {
  // A hidden spend is the dangerous direction. Order must not matter.
  for (answers, behind) in [
    (
      vec![
        ("A".into(), Some(unspent())),
        ("B".into(), Some(spent_by("x"))),
      ],
      "A",
    ),
    (
      vec![
        ("A".into(), Some(spent_by("x"))),
        ("B".into(), Some(unspent())),
      ],
      "B",
    ),
  ] {
    let got = reconcile_outspend("lock", 0, answers).unwrap();
    assert!(got.outspend.spent, "a reported spend must win");
    assert_eq!(got.outspend.txid.as_deref(), Some("x"));
    // The contesting provider is the one that called it unspent — the one a
    // caller would want named as behind, not the one that was believed.
    assert_eq!(got.contested_by.as_deref(), Some(behind));
  }
}

#[test]
fn outspend_agreeing_on_one_spender_is_not_a_disagreement() {
  let got = reconcile_outspend(
    "lock",
    0,
    vec![
      ("A".into(), Some(spent_by("same"))),
      ("B".into(), Some(spent_by("same"))),
    ],
  )
  .unwrap();
  assert_eq!(got.outspend.txid.as_deref(), Some("same"));
  assert!(got.contested_by.is_none());
}

#[test]
fn outspend_keeps_the_most_advanced_answer_about_one_spender() {
  // Both providers name the same spender; one has seen it confirmed, the other
  // only in the mempool. A watchtower asking how deep the spend is must not
  // get "mempool" merely because that provider was listed last.
  let mempool_spend = Outspend {
    spent: true,
    txid: Some("x".into()),
    status: None,
  };
  for answers in [
    vec![
      ("A".into(), Some(spent_by("x"))),
      ("B".into(), Some(mempool_spend.clone())),
    ],
    vec![
      ("A".into(), Some(mempool_spend.clone())),
      ("B".into(), Some(spent_by("x"))),
    ],
  ] {
    let got = reconcile_outspend("lock", 0, answers).unwrap();
    assert_eq!(got.outspend.txid.as_deref(), Some("x"));
    assert_eq!(
      got.outspend.status.map(|s| s.block_height),
      Some(Some(100)),
      "the confirmed answer must win in either order"
    );
  }
}

#[test]
fn outspend_with_no_answer_at_all_is_an_error_not_unspent() {
  // Every provider 404ing the funding transaction is not "the output is
  // unspent" — it is "nobody could tell us".
  let err = reconcile_outspend("lock", 0, vec![("A".into(), None)]).unwrap_err();
  assert!(matches!(err, Error::NotFound { .. }), "{err}");
}

// ── tx ──────────────────────────────────────────────────────────────────────

#[test]
fn tx_bails_when_confirmed_heights_disagree() {
  let err = reconcile_tx(
    "t",
    vec![
      ("A".into(), Some(info(true, Some(800_000)))),
      ("B".into(), Some(info(true, Some(800_001)))),
    ],
  )
  .unwrap_err();
  assert!(matches!(err, Error::HeightDisagreement { .. }), "{err}");
}

#[test]
fn tx_prefers_the_most_advanced_answer_and_lag_is_not_disagreement() {
  for answers in [
    vec![
      ("A".into(), None),
      ("B".into(), Some(info(false, None))),
      ("C".into(), Some(info(true, Some(42)))),
    ],
    vec![
      ("A".into(), Some(info(true, Some(42)))),
      ("B".into(), Some(info(false, None))),
      ("C".into(), None),
    ],
  ] {
    let got = reconcile_tx("t", answers).unwrap().expect("some answer");
    assert!(got.status.confirmed);
    assert_eq!(got.status.block_height, Some(42));
  }
}

#[test]
fn tx_confirmed_without_a_height_is_not_a_fork() {
  // A provider can report `confirmed` while omitting `block_height`. That says
  // nothing about WHERE, so it cannot contradict a provider that named a
  // block — and it must not displace that answer either, in either order.
  for answers in [
    vec![
      ("A".into(), Some(info(true, Some(42)))),
      ("B".into(), Some(info(true, None))),
    ],
    vec![
      ("A".into(), Some(info(true, None))),
      ("B".into(), Some(info(true, Some(42)))),
    ],
  ] {
    let got = reconcile_tx("t", answers).unwrap().expect("some answer");
    assert_eq!(got.status.block_height, Some(42));
  }
}

#[test]
fn tx_unknown_to_everyone_is_none_not_an_error() {
  assert!(
    reconcile_tx("t", vec![("A".into(), None), ("B".into(), None)])
      .unwrap()
      .is_none()
  );
}

// ── tx_hex ──────────────────────────────────────────────────────────────────

#[test]
fn hex_from_one_provider_beats_another_s_404() {
  // The regression this exists for: a sequential path returned the FIRST
  // response that was success *or* 404, so a lagging provider listed first
  // collapsed the read to "never heard of it".
  let got = reconcile_hex(
    "t",
    vec![("A".into(), None), ("B".into(), Some("beef".into()))],
  )
  .unwrap();
  assert_eq!(got.as_deref(), Some("beef"));
}

#[test]
fn hex_disagreement_is_an_error() {
  let err = reconcile_hex(
    "t",
    vec![
      ("A".into(), Some("aa".into())),
      ("B".into(), Some("bb".into())),
    ],
  )
  .unwrap_err();
  assert!(matches!(err, Error::BytesDisagreement { .. }), "{err}");
}

#[test]
fn hex_is_trimmed_so_whitespace_is_not_a_disagreement() {
  let got = reconcile_hex(
    "t",
    vec![
      ("A".into(), Some("beef\n".into())),
      ("B".into(), Some("  beef  ".into())),
    ],
  )
  .unwrap();
  assert_eq!(got.as_deref(), Some("beef"));
}

#[test]
fn hex_unknown_to_everyone_is_none() {
  assert!(
    reconcile_hex("t", vec![("A".into(), None)])
      .unwrap()
      .is_none()
  );
}

// ── tip ─────────────────────────────────────────────────────────────────────

#[test]
fn tip_drops_a_liar_above_the_median_when_there_are_three() {
  assert_eq!(agreed_tip(vec![800_000, 800_001, 900_000]), Some(800_001));
}

#[test]
fn tip_keeps_honest_skew_inside_the_window() {
  let heights = vec![800_000, 800_000 + MAX_TIP_SKEW, 800_001];
  assert_eq!(agreed_tip(heights), Some(800_000 + MAX_TIP_SKEW));
}

#[test]
fn tip_outlier_filter_is_a_no_op_at_two_providers() {
  // With two, the median IS the higher — so a liar above it is never dropped.
  // This documents the limitation rather than pretending it away.
  assert_eq!(agreed_tip(vec![800_000, 900_000]), Some(900_000));
}

#[test]
fn a_single_provider_is_its_own_tip() {
  assert_eq!(agreed_tip(vec![800_000]), Some(800_000));
}

#[test]
fn no_heights_at_all_is_none() {
  assert_eq!(agreed_tip(vec![]), None);
}

// ── collecting answers ──────────────────────────────────────────────────────

#[test]
fn one_provider_answering_is_enough() {
  let (answers, failures) = policy::collect::<u64, &str>(
    "/x",
    vec![("A".into(), Err("down")), ("B".into(), Ok(Some(7)))],
  )
  .unwrap();
  assert_eq!(answers, vec![("B".to_string(), Some(7))]);
  assert_eq!(failures.len(), 1, "the partial outage is still reported");
}

#[test]
fn every_provider_failing_is_an_error_naming_all_of_them() {
  let err = policy::collect::<u64, &str>(
    "/x",
    vec![("A".into(), Err("down")), ("B".into(), Err("also down"))],
  )
  .unwrap_err();
  let msg = err.to_string();
  assert!(matches!(err, Error::NoProviderAnswered { .. }));
  assert!(msg.contains('A') && msg.contains('B'), "{msg}");
}

#[test]
fn a_404_is_an_answer_not_a_failure() {
  let (answers, failures) =
    policy::collect::<u64, &str>("/x", vec![("A".into(), Ok(None))]).unwrap();
  assert_eq!(answers, vec![("A".to_string(), None)]);
  assert!(failures.is_empty());
}

// ── quorum ──────────────────────────────────────────────────────────────────

#[test]
fn too_few_providers_is_warned_about() {
  assert!(policy::quorum_warning(1).is_some());
  assert!(policy::quorum_warning(2).is_some());
  assert!(policy::quorum_warning(3).is_none());
}

// ── provider roots ──────────────────────────────────────────────────────────

#[test]
fn normalise_drops_blanks_and_trailing_slashes() {
  // A stray comma in a config list produces an all-space entry. Dropping it
  // matters: kept, it becomes a provider whose every URL starts with a space.
  let got = esplora_quorum::normalise(vec![
    "https://a.example/api/".into(),
    "   ".into(),
    "".into(),
    "  https://b.example/api  ".into(),
  ]);
  assert_eq!(got, ["https://a.example/api", "https://b.example/api"]);
}

#[test]
fn normalise_drops_duplicates_so_the_provider_count_means_something() {
  // One host listed three times is one provider. Counted as three it would
  // silence quorum_warning on a deployment that has no quorum at all.
  let got = esplora_quorum::normalise(vec![
    "https://a.example/api".into(),
    "https://a.example/api/".into(),
    "  https://a.example/api ".into(),
  ]);
  assert_eq!(got, ["https://a.example/api"]);
  assert!(policy::quorum_warning(got.len()).is_some());
}

// ── broadcast wordings ──────────────────────────────────────────────────────

#[test]
fn already_known_covers_the_wordings_providers_return() {
  for m in [
    "sendrawtransaction RPC error: txn-already-in-mempool",
    "txn-already-known",
    "Transaction already in block chain",
    "ALREADY IN MEMPOOL",
    "Transaction outputs already in utxo set",
  ] {
    assert!(is_already_known(m), "should be treated as accepted: {m}");
  }
  // A real rejection must not be swallowed as success. Each of these leaves a
  // user without their refund, and each was once logged as "likely already
  // sent" by a bare substring match on "already".
  for m in [
    "bad-txns-inputs-missingorspent",
    "non-BIP68-final",
    "min relay fee not met",
    "dust",
    "txn-mempool-conflict",
    "insufficient fee, the input was already counted",
  ] {
    assert!(!is_already_known(m), "should be a failure: {m}");
  }
}
