# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] — 2026-09-01

### Added

- `/address/:addr/txs`, as `Chain::address_txs` and
  `policy::reconcile_address_txs`. Existence UNIONS and confirmation takes a
  MAJORITY, because the two mistakes a deposit watcher can make do not cost the
  same: saying "confirmed" when it is not ends the user's attention, while
  saying "not yet" only prolongs it.
- `AddressTx` and `Vout`, deliberately narrow — only the fields a watcher reads.
- `Error::ValueDisagreement`, for providers that disagree about what a
  transaction paid. Outputs are fixed by the txid, so this is not lag: one of
  them is wrong about money, and the caller must not pick a side.

### Note

Minor rather than patch: the new variant makes `Error` match arms
non-exhaustive for anyone matching it exhaustively.

## [0.1.0] — 2026-08-13

Initial extraction.

### Added

- `policy` — the reconciliation rules as pure functions: `agreed_tip`,
  `reconcile_tx`, `reconcile_hex`, `reconcile_outspend`, `collect`,
  `is_already_known`, `quorum_warning`. No I/O, so they run anywhere and are
  testable without a network.
- `types` — the Esplora REST shapes (`TxStatus`, `TxInfo`, `Outspend`), the
  `Answer` alias that keeps a 404 distinct from an unreachable provider, and
  `route` helpers for the paths this crate reads.
- `error` — typed errors, so "the chain says no", "we could not ask" and "the
  providers contradict each other" are distinguishable rather than one string.
- `client` (feature) — a ready-made async client over `reqwest`.
- Builds for `wasm32-unknown-unknown` without the `client` feature.

### Notes

Extracted from a deployment that had grown two copies of these rules, one
native and one for wasm. The rules and their tests came across unchanged in
behaviour; the differences between the two copies were factoring, transport and
test coverage rather than policy.

Two behaviours were settled rather than carried:

- **No default providers.** The native copy substituted its own favourites for
  an empty list; the browser copy refused. Refusing wins — which providers to
  trust is the caller's decision, and silently supplying defaults makes a
  misconfigured deployment look configured.
- **Contested spends are reported, not printed.** The native copy wrote to
  stderr when one provider called an output spent and another unspent. That is
  now `Spend::contested_by`, naming the provider that called it unspent, so the
  caller decides whether it is a log line, a metric, or nothing.

[Unreleased]: https://github.com/silkmoney/esplora-quorum/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/silkmoney/esplora-quorum/releases/tag/v0.1.0
