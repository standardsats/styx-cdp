# Architecture

This repo is the testnet implementation of the STYX v1 CDP protocol on Liquid, built on
Simplicity covenants. The protocol itself is specified in `spec-site/` and implemented by the
five frozen covenants in `covenants/`; this codebase builds, signs, and finalizes the
transactions that drive them.

The implementation is a decomposition of a working prototype (the liquid-styx demo repo,
`demos/04-v1-cdp`), not a redesign: transaction layouts, witness structures, and math are ported
verbatim and put under tests.

## Crate graph

The dependency arrows are the architecture. The compiler enforces the layering: `styx-pset`
cannot grow node IO because `elementsd` is not in its dependency tree.

```
styx-node  ->  styx-pset  ->  styx-core
(elementsd     (PSET builders,   (pure: units, math, oracle ticks,
 adapter,       sign, finalize)   domain state, covenant artifacts,
 e2e tests)                       witness encoders)
```

- **styx-core** - everything deterministic and IO-free. Unit newtypes (`Sats`, `Obol`, `Price`,
  `RatioK`, `BlockHeight`) with checked math; the CR formula `coll_at_cr` (exact truncation
  parity with the covenants); oracle tick construction (3-of-5, quorum enforced by the
  constructor); domain state (`VaultState`, `IssuerState`, `ProtocolState` - single pot/reserve
  UTXO is structural, not checked); the frozen covenant sources (embedded via `include_str!`)
  and their compilation into taproot artifacts; and the type-safe witness encoders where the
  `ResolvedType` and the `Value` derive from the same Rust type, so a wrong `Either` nesting is
  a compile error, not a wrong-arm prune at runtime.
- **styx-pset** - pure transaction construction: `(state + intent) -> PSET`, with an explicit
  build -> sign -> finalize pipeline (the Simplicity sighash depends on the complete tx body, so
  the body is fixed first, signatures come second, covenant witnesses are pruned last). Builders
  refuse to build anything the covenants would reject: a prune rejection of a builder-produced
  tx is a bug by definition, and the test suite asserts that equivalence op by op.
- **styx-node** - the only impure crate: elementsd RPC, protocol-state scanning, broadcast with
  retry-on-conflict (every mint serializes through the one issuer UTXO), the regtest harness,
  and the on-node e2e acceptance suite.

## Covenants and the freeze

`covenants/*.simf` are the five frozen v1 covenants (vault, issuer, stability, pot_outflow,
reserve_repay), byte-identical in behaviour to the prototype's `v1-covenant-freeze` tag. The
freeze is enforced by a test, not by convention: `styx-core/src/golden.rs` compiles each source
against fixed dummy params and asserts its CMR against the recorded frozen value. CMRs do not
commit to comments, so doc edits pass; any behavioural edit fails the suite.

## Test pyramid

Most coverage lives in fast, node-free tiers. The key fact making this possible:
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
