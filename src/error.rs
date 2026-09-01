//! Typed errors, so a caller can tell "the chain says no" from "we could not
//! ask" from "the providers contradict each other" — three situations that
//! call for three different responses and were previously one string.

use alloc::string::String;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
  /// Not one provider answered. Distinct from a 404, which is an answer.
  #[error("no esplora provider answered {what}: {failures}")]
  NoProviderAnswered { what: String, failures: String },

  /// Two providers report the same transaction confirmed at different heights.
  /// One is lying or on a fork; there is no safe way to pick.
  ///
  /// Both heights are known: an answer that claims confirmation without naming
  /// a block says nothing to contradict, and never raises this.
  #[error(
    "providers disagree on where {txid} is: {a} says block {a_height}, {b} says block {b_height}"
  )]
  HeightDisagreement {
    txid: String,
    a: String,
    a_height: u64,
    b: String,
    b_height: u64,
  },

  /// Two providers disagree about what a transaction paid an address.
  ///
  /// A transaction's outputs are fixed by its txid, so this is not lag and not
  /// a fork: one of them is wrong about money. Anything downstream that treats
  /// the amount as settled -- a deposit watcher deciding whether enough
  /// arrived -- must not proceed on either answer.
  #[error(
    "providers disagree on what {txid} paid {address}: {a} says {a_value} sat, {b} says {b_value} sat"
  )]
  ValueDisagreement {
    txid: String,
    address: String,
    a: String,
    a_value: u64,
    b: String,
    b_value: u64,
  },

  /// Two providers returned different bytes for the same txid. A transaction's
  /// serialization is determined by its hash, so both cannot be right.
  #[error("providers disagree on the bytes of {txid}: {a} and {b} returned different hex")]
  BytesDisagreement { txid: String, a: String, b: String },

  /// Two providers name different spenders for one outpoint.
  #[error("providers disagree on what spent {outpoint}: {a} says {a_txid:?}, {b} says {b_txid:?}")]
  SpenderDisagreement {
    outpoint: String,
    a: String,
    a_txid: Option<String>,
    b: String,
    b_txid: Option<String>,
  },

  /// Every provider answered, and none of them knows about it.
  #[error("{what} not found")]
  NotFound { what: String },

  /// No provider accepted a broadcast.
  #[error("no provider accepted the broadcast: {failures}")]
  BroadcastRejected { failures: String },

  /// No providers were configured, or all of them were blank.
  #[error("no esplora providers configured")]
  NoProviders,

  /// Transport or decoding failure, from the optional client.
  #[error("{0}")]
  Http(String),
}
