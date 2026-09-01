#![cfg_attr(not(feature = "std"), no_std)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]

//! Read Bitcoin chain state from several Esplora providers without trusting any
//! one of them.
//!
//! Public Esplora providers are interchangeable and individually unreliable.
//! Asking one and believing it is fine until the day it lags, forks, or lies —
//! and the cost is asymmetric: an extra HTTP request costs nothing, while a
//! wrong tip height or a hidden spend can cost a user their money.
//!
//! This crate is the reconciliation, not the fetching. **The core performs no
//! I/O**: you hand it the answers, it hands you a verdict. That is what lets it
//! run in a browser via `wasm32-unknown-unknown` — where `reqwest` and a tokio
//! client do not exist — and against providers you run, or several you do not.
//!
//! # Usage
//!
//! With the `client` feature, one call:
//!
//! ```rust,ignore
//! let chain = esplora_quorum::Chain::new(vec![
//!   "https://blockstream.info/api".into(),
//!   "https://mempool.space/api".into(),
//! ])?;
//! let tip = chain.tip_height().await?;
//! ```
//!
//! Without it, you fetch and the crate decides:
//!
//! ```rust,ignore
//! use esplora_quorum::{policy, types::route};
//!
//! let answers = my_fetch_all(&bases, &route::tx(txid))?;   // your transport
//! let (answers, failures) = policy::collect(&route::tx(txid), answers)?;
//! let info = policy::reconcile_tx(txid, answers)?;
//! ```
//!
//! # The rules
//!
//! Each is a pure function in [`policy`], testable without a network:
//!
//! | Question | Rule |
//! |---|---|
//! | tip height | drop outliers above the median, take the **max** of the rest |
//! | `/tx/:txid` | most advanced answer wins; **different confirmed heights is fatal** |
//! | `/tx/:txid/hex` | one having it beats another's 404; **different bytes is fatal** |
//! | outspend | **spent beats unspent**; different spenders is fatal |
//! | broadcast | fan out to all; any acceptance wins; "already known" counts |
//!
//! # What it does not do
//!
//! It cannot make two providers into three. Below
//! [`policy::MIN_PROVIDERS_FOR_MAJORITY`] the outlier filter is a **no-op** —
//! with two providers the median is the higher of the two — so a single
//! provider reporting a wrong tip decides when a timelock has elapsed. Call
//! [`policy::quorum_warning`] at startup and say so out loud; the remaining
//! protection at two is agreement, not majority.

extern crate alloc;

pub mod error;
pub mod policy;
pub mod types;

#[cfg(feature = "client")]
#[cfg_attr(docsrs, doc(cfg(feature = "client")))]
pub mod client;

pub use error::Error;
pub use types::{
  AddressTx, Answer, Outspend, TxInfo, TxStatus, Vout, confirmations, normalise, route,
};

#[cfg(feature = "client")]
pub use client::Chain;
