# Roadmap

Milestones are TDD-sized: each one starts with its tests and ends with `cargo test --workspace`
green. Statuses: [ ] planned, [x] done.

- [x] **M0 - freeze gate + scaffold.** Workspace, nix toolchain, vendored frozen covenants,
  `golden::frozen_cmrs_match_v1_freeze` reproduces the five CMRs recorded at the v1 covenant
  freeze (2026-07-03).
- [x] **M1 - math core.** `coll_at_cr` vectors, truncation direction, zero-price, overflow width
  (settled: the covenant computes in exact 32x32 -> 64 jets over a u32 debt domain enforced by
  the mint gate, so the port takes u32 cents and cannot overflow), named band constants.
- [x] **M2 - leaves, taproot, addresses, preflight.** Golden data-leaf bytes (45B vault, 5B
  issuer), leaf VC words, NUMS, tapleaf tag, golden spks under test params, deploy preflight
  as a Result (duplicate/negated oracle keys, asset collisions). Stale pins are
  unrepresentable: `Artifacts::compile` wires every pin from the just-compiled sibling;
  checking the compiled pins against the actually-published artifacts is production deploy
  tooling, out of the v1 scope.
- [x] **M3 - witness encoders (type conformance).** `ToSimf` layer, the three op enums and their
  total lowerings; the OP sum types equal the covenant-declared witness types, and every
  variant satisfies its covenant.
- [x] **M4 - prune harness + POKE + OPEN.** styx-pset: intents, TxPlan + witness slots,
  finalize via `satisfy_with_env` (the node's verdict, off node), testkit (test deploy,
  synthetic chain state, tamper, per-slot verdicts); POKE proves the pipeline end to end,
  OPEN ports the borrow-fee probes (E-2). Builders refuse bad intents with typed errors;
  `*_unchecked` variants let negative tests make the covenant the judge.
- [x] **M5 - arm-discrimination matrix.** 8x8 vault, 4x4 issuer, 2x2 stability over canonical
  accepting scenarios for every op; golden sha256 digests of the pruned vault witness per
  variant. Two documented off-diagonal accepts, both sound: an owner-signed CLOSE encoding
  accepts in the full-liq and bad-debt environments (both return the full debt to the pot,
  and CLOSE is deliberately permissive once the owner signs). No permissionless encoding
  accepts off-diagonal.
- [x] **M6 - owner ops.** REPAY / CLOSE / DRAW / REFRESH builders with typed intents (the
  fee policy lives in the types: REPAY/REFRESH carry a mandatory separate fee coin, CLOSE
  and DRAW pay from collateral), `sign::owner_sign` / `install_owner_sig` as the wallet
  seam, and the ported probes: collateral skim, strict-ratchet stale tick, unhealthy
  refresh, refresh-as-draw drain (M-2), wrong owner key. Known follow-up: the matrix
  scenarios for these four ops still hand-build their layouts; fold them onto the builders
  when M7 promotes the liquidation scenarios.
- [x] **M7 - liquidations.** Partial / full-liq / bad-debt as checked builders
  (`bad_debt_reserve_pay` joins the math core); the matrix scenarios now run through the
  builders (golden digests unchanged), only REDEEM stays hand-built until M8. Probes: stale
  tick, under/over-heal, short stability fee, sybil reserve, full-liq band gates, stale vs
  issuer anchor, fake vault at input 0 (finding B), no issuer co-spend, reserve at the wrong
  index, recap bypass, the 20% cap (M-1).
- [x] **M8 - REDEEM + remaining probes.** REDEEM as a checked builder (backing floor
  min(par, backing_k), x in (0, debt], full-debt redemption boundary); the last scenario is
  now builder-backed. Probes: par extraction under an under-backed tick (E-2 tail), poke and
  vault index guards, zero price, d=0 drain (Hole A), decoy pot (Hole B), non-OBOL pot. The
  full inventory is mapped to covenant gates and audit finding ids in tests/probes.rs.
- [x] **M9 - math parity properties.** The covenant's coll_at_cr pasted verbatim into a shim
  program (source-locked against the frozen vault.simf); 1024 proptest cases prove exact
  equality (the Rust value is accepted, the value plus one is not) across the u32 domain
  and the protocol k literals; 64 end-to-end cases prove the heal-band edges are one-sat
  tight through the liquidate builder.
- [x] **M10 - PSET invariants.** to_pset fills witness_utxo and the covenant inputs' taproot
  metadata; sign_funding only produces SIGHASH_ALL and finalize_pset refuses anything else
  (E-5, plus the redirect test: a moved payout kills the signature); non-explicit outputs
  are refused at emission; golden PSET digests per op family. Fragmented pot/reserve is
  unrepresentable at the builder API (ProtocolState holds one of each) - the scanner-side
  refusal lands with M11.
- [x] **M11 - node adapter + e2e.** styx-node: typed RPC client, the regtest deployment
  ceremony (two non-reissuable issuances, artifacts against the real asset ids, reserve
  seed), scan_protocol with asset filtering and the single-UTXO refusal (L-3, proven on
  node with a fragmented reserve), broadcast with the Conflict/Rejected split (the retry
  loop itself belongs to the role daemons). The on-node smoke runs the whole lifecycle
  through the builders - poke, open, repay, draw, refresh, partial liquidate, bad debt,
  full-liq, redeem, close - with three inline negative spot-checks calibrating
  prune verdict == node verdict, the pot back at full supply, and the scanner agreeing
  with the tracked state.

Out of scope for now: role daemons (keeper scheduler, oracle signer service, borrower wallet),
confidential change outputs, FROST / real oracle data sourcing, fee estimation, CLI and
production deploy tooling (including the compiled-pins-vs-published-artifacts check), and any
covenant change.
