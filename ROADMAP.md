# Roadmap

Milestones are TDD-sized: each one starts with its tests and ends with `cargo test --workspace`
green. Statuses: [ ] planned, [x] done.

- [x] **M0 - freeze gate + scaffold.** Workspace, nix toolchain, vendored frozen covenants,
  `golden::frozen_cmrs_match_v1_freeze` reproduces the five CMRs recorded at the prototype's
  `v1-covenant-freeze` tag.
- [x] **M1 - math core.** `coll_at_cr` vectors, truncation direction, zero-price, overflow width
  (settled: the covenant computes in exact 32x32 -> 64 jets over a u32 debt domain enforced by
  the mint gate, so the port takes u32 cents and cannot overflow), named band constants.
- [x] **M2 - leaves, taproot, addresses, preflight.** Golden data-leaf bytes (45B vault, 5B
  issuer), leaf VC words, NUMS, tapleaf tag, golden spks under test params, deploy preflight
  as a Result (duplicate/negated oracle keys, asset collisions). Stale pins are
  unrepresentable: `Artifacts::compile` wires every pin from the just-compiled sibling;
  checking against published artifacts is deploy tooling (M11).
- [ ] **M3 - witness encoders (type conformance).** `ToSimf` layer, the three op enums and their
  total lowerings; every variant type-checks against the covenant's declared witness type.
- [ ] **M4 - prune harness + POKE + OPEN.** Testkit (deploy fixture, chain-state factory,
  tamper, accept/reject asserts); the smallest op proves the build -> sign -> finalize pipeline
  end to end; OPEN ports the borrow-fee probes.
- [ ] **M5 - arm-discrimination matrix.** 8x8 vault, 4x4 issuer, 2x2 stability: encoding of op i
  against the canonical env of op j accepts iff i == j; golden pruned program bytes per variant.
- [ ] **M6 - owner ops.** REPAY / CLOSE / DRAW / REFRESH builders with their negatives
  (collateral skim, stale tick, unhealthy refresh, refresh-as-draw drain, wrong owner key).
- [ ] **M7 - liquidations.** Partial (heal band, fee split, sybil-reserve), full-liq (band
  gates), bad-debt (attest, fake vault, no-issuer, wrong reserve index, recap bypass, 20% cap).
- [ ] **M8 - REDEEM + remaining probes.** Backing-ratio floor, poke index guard, zero price,
  d=0 drain, decoy pot, asset confusion, second-vault alias. Full probe inventory mapped to
  prototype lines and audit finding ids.
- [ ] **M9 - math parity properties.** Verbatim covenant shim for `coll_at_cr` (with an
  anti-drift source check), proptest exact-value parity, heal-band edge tightness.
- [ ] **M10 - PSET invariants.** Golden PSETs, finalize == raw tx, blinded-output and
  fragmented-state refusals, E-5 keeper payout binding (SIGHASH_ALL, redirect test).
- [ ] **M11 - node adapter + e2e.** Protocol scanner (single-UTXO enforcement), broadcast with
  retry-on-conflict, regtest harness, full lifecycle smoke, prune-vs-node calibration
  spot-checks.

Out of scope for now: role daemons (keeper scheduler, oracle signer service, borrower wallet),
confidential change outputs, FROST / real oracle data sourcing, fee estimation, CLI tooling, and
any covenant change.
