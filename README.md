# STYX v1

A collateralized debt position (CDP) protocol on Liquid, built on Simplicity covenants.
Lock L-BTC collateral in a covenant-enforced vault, mint the OBOL stable asset against it,
keep the peg through permissionless liquidations, redemptions, and a stability reserve -
with no multisig, no federation, and no admin keys. The rules are the covenants.

- `spec-site/` - the human-readable protocol specification (architecture, lifecycle, oracle
  model, reserve economics, security analysis).
- `covenants/` - the five frozen v1 SimplicityHL covenants. Behaviourally frozen; a golden-CMR
  test enforces it.
- `crates/` - the implementation: pure protocol core, PSET transaction builders, node adapter.
  See [ARCHITECTURE.md](ARCHITECTURE.md).
- [ROADMAP.md](ROADMAP.md) - milestone status.

## Building

```
nix develop            # rustc, cargo, and a Simplicity-capable elementsd (ELEMENTSD_EXE)
cargo test --workspace # fast tiers: unit + prune-level, no node needed
cargo test -p styx-node -- --ignored   # on-node e2e (regtest)
```

Without nix: rustc >= 1.88 builds the fast tiers; the e2e tier needs an elementsd built from
the ElementsProject `simplicity` branch, pointed to by `ELEMENTSD_EXE`.
