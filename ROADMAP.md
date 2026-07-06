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

The v1 implementation (M0-M11) is complete. The role-daemon phase (R0-R5 below) turns it into
a live multi-machine system: separate oracle, wallet, and keeper daemons on a private Elements
chain, with Nostr as the oracle quote transport. Design context: SETUP.md (added in R5) and the
deploy/ configs.

- [x] **R0 - private-chain infra + styx-deploy.** styxnet is a custom Elements chain (same
  CCustomParams path as liquidregtest, so generatetoaddress and the Simplicity evbparam work
  unchanged; verified on a two-node host spike: equal genesis, Simplicity active, funded
  follower). deploy/ carries the producer/follower elements.conf templates (validatepegin,
  anyonecanspendaremine, [styxnet]-section and pchmessagestart gotchas) and produce-blocks.sh.
  styx-node's Node is now URL-based (from_url / from_elementsd / for_wallet); the ceremony
  moved to ceremony.rs. styx-deploy: `run` executes the ceremony against a live node and fills
  styxnet.toml; `verify` recompiles the artifacts and locates the singletons on chain - the
  compiled-pins-vs-published-artifacts check.
- [x] **R1 - styx-watch: the chain indexer.** Every transition inferred from transaction
  layout (no witness decoding): ceremony births, issuer ops by the covenant-pinned token
  successor output (0 POKE / 4 OPEN / 3 DRAW and ATTEST, split by the reserve co-spend),
  vault ops by input signature; builder-conventional positions are verified on top and any
  mismatch degrades to an Anomaly, never a false record. Committed heights (anchor, vault
  last_height) are recovered by a bounded descending scan against the successor's derived spk
  - nLockTime is only their upper bound, check_lock_height being a lower bound on the tx
  (the one undecidable spot, an opaque vault's REFRESH, records an upper bound).
  The owner is the one field a layout cannot yield, so the indexer takes
  candidate owner keys (the wallet's filter seam) and verifies the derived vault spk against
  the actual output - no match tracks the vault as opaque (bookkept, not builder-consumable);
  a successor-spk mismatch or an unrecognizable spend marks it lost. Partial LIQUIDATE and
  REDEEM are layout-isomorphic (both keep last_height) and are reported as one Deleveraged
  event. JSON snapshot (atomic tmp+rename), catch-up scan with a reorg check. The e2e smoke
  now runs through the extracted `lifecycle` driver whose trace is the anchor: reindexing the
  smoke chain from genesis reproduces every step at its txid and converges to the tracked
  state; dust/fragments proven inert on node and off.
- [x] **R2 - quotes: styx-oracle + styx-watch::quotes.** The wire quote is JSON {slot,
  height, price, backing_k, sig} where sig is the protocol BIP340 signature over the tick
  digest - the Nostr key is carriage only, so a quote is trusted for what it proves, not for
  who relayed it. QuoteBook verifies against the covenant oracle keys BEFORE caching (foreign
  signature / transplanted height / out-of-range slot / stale / future / covenant-invalid
  zero price all rejected trace-free - one byzantine oracle must not jam assembly) and
  assembles 3-of-5 ticks grouped by backing_k; an assembled tick finalizes a POKE
  at the prune tier, closing the loop. Transport sits behind QuoteTransport: an in-memory hub
  for tests and the Nostr impl (addressable kind 33321, d = height so relays keep the latest
  quote per (oracle, height), explicit created_at bump for replacement, NIP-40 expiry as a GC
  hint), proven against the embedded LocalRelay: five publishers, live stream + stored-events
  fetch, replacement not accumulation, stranger authors dropped. styx-oracle: per-slot config,
  block loop (one quote per new tip), axum admin (/health, /quote?height, POST /price with a
  zero-price refusal) - the same router and loop the tests drive.
- [x] **R3 - styx-wallet (CLI).** The owner's wallet as a thin CLI over a tested library:
  keygen/address/fund/status/open/repay/draw/refresh/close/redeem. One key-path p2tr script
  (the funding key) holds every coin - L-BTC and OBOL principal/change alike - discovered by
  confirmed-utxo scans and signed via the M10 PSET pipeline (sign_funding, SIGHASH_ALL); the
  owner key never appears as an address, only inside vault commitments, and our vaults are
  whatever the R1 indexer resolves to it (session = styxnet.toml + genesis cross-check +
  snapshot). OPEN's frozen layout needs exact funding, so the wallet shapes an exact coin
  with a self-spend and chains the open on it in the mempool. Ticks are injected at the lib
  boundary; the binary assembles them from the relays (fetch at the tip, walking back to the
  first quorum). The on-node acceptance runs the full owner cycle - fund, shaped open, repay,
  draw, refresh, redeem, close - with real keys, ending with the pot at full supply, the
  collateral back, and a fresh session from the snapshot agreeing state-for-state.
- [ ] **R4 - styx-keeper.** A pure decide(vault, tick, config) -> Action ladder
  (bad-debt / full-liq / partial with plan_partial, property-tested against the builder) plus
  the watchtower duties (poke the issuer toward the tip, refresh dormant healthy vaults, M-2)
  and retry-on-conflict. On-host smoke: wallet opens, oracles cheapen, keeper liquidates.
  Design decision to settle first - owner bytes for foreign vaults (liquidation needs the
  full vault state, and layout inference cannot yield the owner by construction): candidate
  A is a bit-granular sliding-window search of the OPEN transaction's issuer witness for a
  32-byte word w with vault_spk(debt, w, last_height) == the vault's spk - trustless (the
  address commitment judges, exactly like the wallet's candidate filter), no Simplicity
  decoding, sub-second once per vault birth; candidate B restricts the keeper to owners
  known from config. A no-match under A degrades to today's opaque tracking.
- [ ] **R5 - multi-machine assembly.** nostr-rs-relay in the flake, per-role configs/units,
  SETUP.md (bring-up across 3-4 machines), a scenario script (POST /price crash -> observe
  the cascade), and a same-host swarm rehearsal.

Out of scope for this phase: FROST / multisig of the oracle protocol key, a real price feed
(behind a PriceSource trait), authentication of the oracle admin surface (it signs on demand
and moves the price, so it binds loopback / trusted LAN until then), confidential change
outputs, and automated redeem arbitrage by the keeper (the decision ladder leaves room to
add it).
