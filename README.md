# esplora-quorum

Read Bitcoin chain state from several Esplora providers without trusting any one
of them.

Public Esplora providers are interchangeable and individually unreliable. Asking
one and believing it is fine until the day it lags, forks, or lies — and the
cost is asymmetric: an extra HTTP request costs nothing, while a wrong tip
height or a hidden spend can cost a user their money.

This crate is the **reconciliation**, not the fetching. The core performs no
I/O: you hand it the answers, it hands you a verdict. That is what lets it run
in a browser via `wasm32-unknown-unknown` — where `reqwest` and a tokio client
do not exist — and against providers you run, or several you do not.

## Status

Pre-release. See [CHANGELOG.md](CHANGELOG.md).

## Usage

With the `client` feature, one call:

```rust,ignore
let chain = esplora_quorum::Chain::new(vec![
  "https://blockstream.info/api".into(),
  "https://mempool.space/api".into(),
  "https://btcscan.org/api".into(),
])?;

if let Some(warning) = chain.quorum_warning() {
  eprintln!("{warning}");
}

let tip = chain.tip_height().await?;
let depth = chain.confirmations(txid, tip).await?;
```

Without it, you fetch and the crate decides:

```rust,ignore
use esplora_quorum::{policy, route};

let path = route::tx(txid);
let answers = my_fetch_from_every_provider(&bases, &path)?;  // your transport
let (answers, failures) = policy::collect(&path, answers)?;
let info = policy::reconcile_tx(txid, answers)?;
```

`failures` is returned rather than logged, so partial outages surface however
your application surfaces things.

## The rules

Each is a pure function in `policy`, testable without a network.

| Question | Rule |
|---|---|
| tip height | drop outliers more than `MAX_TIP_SKEW` above the median, take the **max** of the rest |
| `/tx/:txid` | most advanced answer wins; **different confirmed heights is fatal** |
| `/tx/:txid/hex` | one provider having it beats another's 404; **different bytes is fatal** |
| `/tx/:txid/outspend/:vout` | **spent beats unspent**; different spenders is fatal |
| broadcast | fan out to all; any acceptance wins; "already known" counts as accepted |

They are asymmetric on purpose, because the failure modes are.

**The tip leans high.** Being early costs a rejected retry; being late costs a
time-sensitive transaction its race.

**Lag is not disagreement.** "Never heard of it", "in the mempool" and
"confirmed" are ordinary propagation states. Treating them as a conflict makes a
watcher flap red on every broadcast. Two providers claiming *different confirmed
heights* is a different matter — one is lying or on a fork, and there is no safe
way to pick.

**A hidden spend is the dangerous direction.** Any provider reporting an output
spent is believed over one reporting it unspent, because an invisible spend is
how a punishing transaction stays unnoticed while you re-broadcast something
that can no longer win. The disagreement is reported rather than swallowed:
`Spend::contested_by` names the provider that saw it.

**A 404 is an answer.** `None` from this crate always means "the chain says no",
never "we could not ask". Not one provider answering is an error naming all of
them, because "all down" and "one down" call for different responses.

**"Already known" is not a rejection.** Providers surface bitcoind's own
wording, which differs between versions and between mempool-acceptance and
already-mined cases. Matched against the known wordings rather than the bare
word "already" — a substring match swallows real rejections that merely mention
it, and callers use this to decide whether a failed broadcast is routine or an
emergency.

## What this does not do

It cannot make two providers into three.

Below `MIN_PROVIDERS_FOR_MAJORITY` the outlier filter is a **no-op**: with two
providers the median is the higher of the two, so a liar above it is never
dropped. `quorum_warning()` says so; call it at startup and print it. At two
providers the remaining protection is agreement, not majority — you will learn
that they disagree, but not which one is right.

It also does not verify anything cryptographically. It reconciles what several
sources claim. A provider that lies consistently, and alone, is believed.

## Development

A flake pins the toolchain from `rust-toolchain.toml`, so the dev shell and the
checks agree with what `cargo` resolves outside Nix.

```sh
nix develop                         # rust 1.96 with the wasm32 target, treefmt, cargo-deny
nix flake check                     # tests, clippy, wasm32, no_std, rustdoc and formatting, sandboxed
nix fmt                             # rustfmt + nixfmt
cargo deny --all-features check     # licences, advisories, sources
```

There is no `nix build`: this is a library with no binary, and downstream Rust
code depends on it through cargo rather than through Nix.

`nix develop` installs a pre-commit hook (formatting) and a pre-push hook (the
fast local gate), leaving any hook of your own in place; `FULL=1 git push` runs
`nix flake check` as well.

Without Nix, the cargo commands below are the whole suite.

## Testing

```sh
cargo test                          # the rules, no network
cargo test --features client        # and the client
cargo check --target wasm32-unknown-unknown --no-default-features
```

## Provenance

Extracted from SilkMoney's atomic-swap watchtower and browser swap driver, which
had grown two copies of these rules — one native, one for wasm — that drifted in
factoring and test coverage rather than in behaviour. Every rule here was a real
failure mode in that deployment before it was a test:

- a lagging provider listed first collapsed a lookup to "never heard of it"
- a hidden spend let a punishing transaction stay invisible
- a bare substring match on "already" logged real rejections as success

## Licence

MIT OR Apache-2.0.
