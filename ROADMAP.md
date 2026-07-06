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
- [x] **R4 - styx-keeper.** Owner bytes for foreign vaults: candidate A SETTLED and landed
  in the indexer - a bit-granular sliding-window search of the OPEN's issuer-input witness,
  each 32-byte window verified against the vault's address commitment (trustless, exactly
  like the wallet's candidate filter; no Simplicity decoding; a miss degrades to opaque
  tracking), so a keeper carries full builder-consumable state for every vault born on
  chain. The pure ladder decide(vault, tick, refresh_lag) mirrors the covenant bands at the
  max quote (bad-debt < 100% strict, full-liq [100%, 115%] inclusive, partial under the
  strict 130% gate, refresh for healthy-but-stale, ratcheted ticks decide nothing);
  plan_partial binary-searches the largest dd whose max extraction fits under the heal-band
  ceiling, residual clamped to the band floor, profitability mirrored. Property tier: every
  verdict is accepted by its checked builder and dd+1 is refused. The keeper itself is a
  purse (the wallet machinery reused) plus a verified book: one action per step (singleton
  successors are unconfirmed until the next block), priority bad-debt > full-liq > partial >
  poke > refresh, conflict-retry that resyncs and rebuilds (a vanished target is a lost
  race, a Rejected is an invariant-break alert - policy unit-tested off node). The smoke
  runs the escalation end to end: the wallet opens, the keeper resolves the foreign owner
  from the witness, pokes, refreshes, heals the $50k dip partially, closes the $35k crash
  as bad debt, and ends compensated with the pot at full supply.
- [x] **R5 - multi-machine assembly.** nostr-rs-relay in the flake; deploy/ carries the
  per-role config templates (oracle / wallet / keeper / relay / styxnet skeleton) and
  systemd units; SETUP.md walks the 3-4 machine bring-up (consensus-block discipline,
  genesis cross-checks, firewall posture, per-role steps); scenario-crash.sh moves every
  oracle's price through the admin endpoints. The acceptance is deploy/swarm.sh: the whole
  topology as real processes on one host - styxnet elementsd + block loop, nostr-rs-relay,
  five freshly-keyed oracle daemons, the ceremony, a funded wallet opening a vault and
  handing the principal to the keeper, then the crash cascade ($50k partial heal, $35k
  bad-debt closure, pot back at full supply), exit 0 only on the full postcondition. The
  rehearsal surfaced and fixed a real liveness bug: a daemon polling faster than blocks
  confirm rebuilt its own liquidation and died on "already in block chain" - broadcast is
  now idempotent (an already-known tx is our txid, not a rejection) and the keeper keeps an
  acted-list so a vault with a pending spend sits out until it confirms (bounded, so an
  evicted transaction retries). New seams the swarm needed: `styx-wallet send` (OBOL/L-BTC
  to any address; the keeper smoke now covers it on node) and `styx-oracle --keygen`.

The role-daemon phase is complete. The testnet phase (T0-T2 below) takes the system to the
public Liquid testnet, where strangers open vaults and liquidate each other.

- [x] **T0 - testnet seams + wallet conflict retry.** The mining assumptions are gone:
  broadcast helpers only broadcast, and the ceremony takes a Confirm mode - self-mine on a
  lone regtest node, await-confirmation where someone else makes blocks (the swarm now
  runs its producer first and rehearses exactly the path a federation-chain deployment
  takes; styx-deploy defaults to awaiting, `--self-mine` for the lone-node case). The
  indexer starts at the deployment anchor from styxnet.toml instead of genesis (the real
  testnet is millions of blocks deep; the reindex e2e proves anchor-start equals the full
  scan). Address display/parse follows network.chain (ex / tex / ert). The conflict retry
  moved into the wallet as the shared policy - resync, rebuild, bounded attempts - with a
  LostRace split: benign for a keeper (someone else did the work), an error for an owner
  (a vault dissolving mid-op wants eyes); the issuer singleton serializes every open and
  draw globally, and the owner-cycle e2e now manufactures that contention deterministically
  (a poke lands under a stale wallet view; the eventual draw conflicts, resyncs, lands).
- [x] **T1 - price feed with scenario override.** The `PriceSource` seam is a trait with
  five exchange backends (Coinbase / Binance / Kraken / Bitstamp / Bitfinex - one per slot,
  so the quorum's divergence reflects genuinely independent sources), polled over public
  spot tickers: at one quote per block, a few-second poll is far below the staleness that
  matters, and every poll is a fresh connection (a websocket backend would implement the
  same trait). Parsers are pure and fast-tier-tested on canned exchange bodies, with sanity
  bounds; a failed poll keeps the last price and /health surfaces the feed age. The
  effective price layers a sticky override on top of the feed: POST /price pins a staged
  crash that holds still while the market ticks underneath, DELETE /price releases it
  (scenario-crash.sh grew the `feed` argument). The endpoint is configurable per oracle,
  which is also the test seam - the integration test runs the whole loop against a local
  mock exchange, no network in any test tier. Systemic staleness (all five backends silent
  at once - one datacenter, a regional block, DNS) is a decided trade-off, not an
  oversight: the opt-in `max_age_secs` gate makes a quiet oracle WITHHOLD quotes, so
  correlated silence degrades the quorum into a safe freeze instead of five oracles
  re-signing a frozen price the covenants cannot tell from a live one; the operator
  override still publishes (scenario control beats a dead feed).
- [x] **T2 (in-repo half) - public-infrastructure tooling and docs.** TESTNET.md is the
  stranger-facing quickstart (faucet straight to the unblinded funding address -
  confidential coins cannot enter the raw funding paths; open at --cr from the live
  price; run a keeper and hunt); deploy/ grew the Liquid-testnet node config, the
  liquid-testnet.toml skeleton, the Caddyfile terminating wss:// for the relay,
  check-health.sh (oracles publishing / feeds fresh / heights in lockstep / relay up) with
  the styx-monitor systemd timer, and soak.sh - the swarm on LIVE exchange feeds with
  health checks throughout and the cascade at prices derived from the live one. SETUP.md
  documents the testnet deployment differences (federation blocks, faucet bootstrap, TLS
  relay, publish the completed config). The network tier exists and ran: live_feeds
  validates every reachable exchange against today's real API (3/5 reachable from the dev
  sandbox, all parsed), and a live soak passed end to end - three exchanges quoting
  genuinely divergent prices, the cascade derived from the market, the oracles returned to
  it after.
- [ ] **T2 (ops half) - the actual deployment.** Hosts for the five oracles (one exchange
  each, max_age_secs armed) + infra (relay behind the Caddyfile, monitoring timer wired to
  a notifier); DNS + TLS; the ceremony on Liquid testnet (faucet-funded deploy wallet,
  getdeploymentinfo preflight, no --self-mine) and verify; publish the completed
  liquid-testnet.toml and fill the URL placeholders in TESTNET.md; OBOL distribution for
  early keepers (a treasury vault + `send`); the multi-day soak with live_feeds green on
  every oracle host before inviting anyone.

Out of scope for these phases: FROST / multisig of the oracle protocol key, authentication
of the oracle admin surface (it signs on demand and moves the price, so it binds loopback /
trusted LAN until then), confidential change outputs, and automated redeem arbitrage by the
keeper (the decision ladder leaves room to add it).
