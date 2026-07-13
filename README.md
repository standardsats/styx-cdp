# STYX v1

A collateralized debt position (CDP) protocol on Liquid, built on Simplicity covenants.
Lock L-BTC collateral in a covenant-enforced vault, mint the OBOL stable asset against it,
keep the peg through permissionless liquidations, redemptions, and a stability reserve -
no multisig, federation, or admin keys.

- `spec-site/` - the human-readable protocol specification (architecture, lifecycle, oracle
  model, reserve economics, security analysis).
- `covenants/` - the five frozen v1 SimplicityHL covenants. Behaviourally frozen; a golden-CMR
  test enforces it.
- `crates/` - the implementation: pure protocol core, PSET transaction builders, node adapter,
  and the role daemons (oracle, wallet, keeper). See [ARCHITECTURE.md](ARCHITECTURE.md).
- [GUIDE.md](GUIDE.md) - the hands-on walkthrough: deploy, oracles, a vault and every op on
  it, the keeper, a price-crash liquidation.
- [TESTNET.md](TESTNET.md) - **start here to try it:** the explorer, the local app (a web
  UI for the wallet and the keeper), or the CLI, against the public Liquid-testnet
  deployment - `deploy/join-testnet.sh` sets it all up, then faucet, a vault, and hunting
  other people's positions.
- [SETUP.md](SETUP.md) - multi-machine network bring-up; `deploy/swarm.sh` rehearses the
  whole topology on one host.
- [DEPLOY.md](DEPLOY.md) - the operator's deploy-day runbook for the public testnet
  (DNS, oracles, the ceremony, the go-live checklist).
- [ROADMAP.md](ROADMAP.md) - milestone status.

## Building

```
nix develop            # rustc, cargo, and a Simplicity-capable elementsd (ELEMENTSD_EXE)
cargo test --workspace # fast tiers: unit + prune-level, no node needed
cargo test -p styx-node -- --ignored   # on-node e2e (regtest)
```

Without nix: rustc >= 1.88 builds the fast tiers; the e2e tier needs an elementsd built from
the ElementsProject `simplicity` branch, pointed to by `ELEMENTSD_EXE`.

## Provenance

Commits and release tags are signed by the maintainer key ([charon.asc](charon.asc)):

```
Χάρων <charon@styx.network>
53DF BF82 6E12 6804 50FD  51D6 9790 D5F0 CB43 FE12
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Unless you explicitly state otherwise,
any contribution intentionally submitted for inclusion in this work by you, as
defined in the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.
