# Architecture

This repo is the testnet implementation of the STYX v1 CDP protocol on Liquid, built on
Simplicity covenants. The protocol itself is specified in `spec-site/` and implemented by the
five frozen covenants in `covenants/`; this codebase builds, signs, and finalizes the
transactions that drive them.

Transaction layouts, witness structures, and math follow the frozen covenants exactly and are
put under tests that make the covenants themselves the judge.

## Crate graph

The compiler enforces the layering: `styx-pset` cannot grow node IO because `elementsd` is not
in its dependency tree.

```
             styx-deploy / styx-oracle / styx-keeper / styx-wallet   (role daemons, R phase)
                    |               \
                 styx-watch  ->  styx-node  ->  styx-pset  ->  styx-core
                 (daemon base:   (elementsd    (PSET builders,   (pure: units, math,
                  styxnet.toml,   adapter,      sign, finalize)   oracle ticks, domain,
                  chain indexer,  scan,                           covenant artifacts,
                  quote client)   broadcast,                      witness encoders)
                                  regtest harness)
```

The bottom three crates are the frozen v1 implementation (M0-M11). The role-daemon phase
(R0-R5, see ROADMAP.md) adds `styx-watch` (the shared daemon base: the `styxnet.toml`
deployment config, the chain indexer, and - landing in R2 - the Nostr quote client) and
the four binaries. Tokio / axum / reqwest / nostr-sdk / clap live only in these new crates, so
the core stays minimal.

- **styx-core** - everything deterministic and IO-free. Unit newtypes (`Sats`, `Obol`, `Price`,
  `RatioK`, `BlockHeight`) with checked math; the CR formula `coll_at_cr` (exact truncation
  parity with the covenants); oracle tick construction (3-of-5, quorum enforced by the
  constructor); domain state (`VaultState`, `IssuerState`, `ProtocolState` - the single pot/reserve
  UTXO holds by construction); the frozen covenant sources (embedded via `include_str!`)
  and their compilation into taproot artifacts; and the type-safe witness encoders where the
  `ResolvedType` and the `Value` derive from the same Rust type, so a wrong `Either` nesting is
  a compile error, not a wrong-arm prune at runtime.
- **styx-pset** - pure transaction construction: `(state + intent) -> PSET`, with an explicit
  build -> sign -> finalize pipeline (the Simplicity sighash depends on the complete tx body, so
  the body is fixed first, signatures come second, covenant witnesses are pruned last). Builders
  refuse to build anything the covenants would reject: a prune rejection of a builder-produced
  tx is a bug, and the test suite asserts that equivalence op by op.
- **styx-node** - the elementsd adapter: a URL-based RPC client (`from_url` / `from_elementsd`
  / `for_wallet`), protocol-state scanning with the single-UTXO refusal, broadcast with the
  Conflict/Rejected split (every mint serializes through the one issuer UTXO), the deployment
  ceremony (`ceremony.rs`, reused by the regtest harness and styx-deploy), and the on-node e2e
  acceptance suite.
- **styx-watch** - the shared base of the role daemons: the `styxnet.toml` config type; the
  chain indexer (`index.rs`) that reads the whole protocol state machine off transaction
  layouts - covenant-pinned token successor outputs, pot deltas, and committed heights
  recovered by spk-matched scans (nLockTime only bounds them from above) - with candidate
  owner keys as the wallet's filter seam (a vault with no matching candidate is tracked
  opaque, an unrecognizable spend of a known vault is marked lost); the atomic JSON snapshot
  (`snapshot.rs`); the RPC catch-up scan (`sync.rs`); the quote client - a verified book
  (`quotes.rs`: every wire quote checked against the covenant oracle keys before caching,
  3-of-5 tick assembly grouped by backing_k) behind the `QuoteTransport` seam
  (`transport.rs`: in-memory hub for tests; `nostr.rs`: addressable events with d = height,
  replacement semantics, author filtering as spam control - the protocol signature is the
  trust root, the Nostr identity is carriage).
- **styx-oracle** - one quorum slot as a daemon: signs the configured price at every new
  block of its own elementsd, publishes over Nostr, and serves the debug/admin HTTP surface
  (/health, /quote?height as the fallback channel, POST /price as the scenario lever). The
  price sits behind a `set_price` seam a real feed can drive later.
- **styx-wallet** - the owner's CLI over a tested library. All funds live at one key-path
  p2tr script (the funding key), signed through the PSET pipeline (SIGHASH_ALL); vault
  ownership is the separate owner key, which never appears as an address - the wallet's
  vaults are whatever its indexer sync resolves to it. OPEN's exact-funding requirement is
  met by a shaping self-spend chained in the mempool; ticks come from the relays in the
  binary and are injected at the library boundary in tests.
- **styx-keeper** - the watchtower daemon: a purse (the wallet machinery reused) plus a
  verified quote book. The pure ladder decide(vault, tick) mirrors the covenant bands at
  the max quote and plan_partial sizes the heal (property-tested against the checked
  builders); one action per step, priority bad-debt > full-liq > partial > poke > refresh;
  broadcast conflicts resync-and-rebuild, a Rejected is an invariant-break alert. Foreign
  vault owners come from the indexer's witness-scan recovery, so every vault born on chain
  is a full liquidation target.

## Covenants and the freeze

`covenants/*.simf` are the five frozen v1 covenants (vault, issuer, stability, pot_outflow,
reserve_repay), behaviourally frozen on 2026-07-03. A test enforces
the freeze: `styx-core/src/golden.rs` compiles each source against fixed dummy params
and asserts its CMR against the recorded frozen value. CMRs do not commit to comments, so doc
edits pass; any behavioural edit fails the suite.

## Test pyramid

Most coverage lives in fast, node-free tiers. This works because
`CompiledProgram::satisfy_with_env` prunes a covenant against a fully in-memory spend
environment and returns the same accept/reject verdict the node gives.

1. **Unit** (milliseconds): math vectors and properties, encodings, address derivation, golden
   CMRs.
2. **Prune-level** (no node): build the complete tx plus claimed input UTXOs, construct
   `ElementsEnv`, call `satisfy_with_env`, assert the verdict. All positive op tests, the
   arm-discrimination matrices, the ported attack probes (single-cause negatives: every rejected
   tx differs from an accepted twin by exactly one mutation), and the Rust-vs-covenant math
   parity property tests live here.
3. **On-node e2e** (regtest, `#[ignore]`, needs `ELEMENTSD_EXE`): the full lifecycle smoke test
   plus a handful of negative spot-checks that calibrate "prune verdict == node verdict" for the
   pinned simplicityhl rev.

Run the fast tiers with `cargo test --workspace`; the e2e tier with
`cargo test -p styx-node -- --ignored` inside `nix develop`.

## Toolchain pins

Reproducible covenant compilation -> stable CMRs -> stable addresses. Three pins matter:

- `simplicityhl` at git rev `63cd0924...` (workspace `Cargo.toml`). The prune verdict is the
  test oracle for the whole prune tier; bumping the rev requires re-running the e2e negative
  spot-checks.
- The Simplicity-capable elementsd, built by `nix/elementsd-simplicity.nix` from the
  ElementsProject `simplicity` branch (exposed as `ELEMENTSD_EXE` in the dev shell).
- rustc >= 1.88 (`rust-toolchain.toml` for rustup users; the flake's nixpkgs pin inside
  `nix develop`).
