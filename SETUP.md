# STYX v1 network setup

Bring-up of a private STYX network across 3-4 machines. Every template referenced here
lives in `deploy/`; the one-host dress rehearsal of these exact steps is
`deploy/swarm.sh` - run it first, it catches most mistakes cheaply.

## Topology

```
machine A "infra":    elementsd (block producer) + nostr-rs-relay + styx-deploy (once)
machines B1..B5:      elementsd (follower) + styx-oracle     (5 daemons; 1-2 machines fine)
machine C "user":     elementsd (follower) + styx-wallet (CLI)
machine D "keeper":   elementsd (follower) + styx-keeper     (several keepers also fine)
```

Every daemon talks only to ITS OWN elementsd (localhost RPC) and to the Nostr relay. The
one shared artifact is `styxnet.toml`: the operator writes the skeleton, `styx-deploy run`
completes it on machine A, and the completed file is copied to every machine. Every
session cross-checks its genesis against the local node before signing anything, so a
machine pointed at the wrong chain refuses to act.

## Prerequisites, every machine

- The Simplicity-capable elementsd (the flake's `packages.elementsd`; `nix build .#elementsd`).
- The role binaries (`cargo build --release`, or copy from a build machine):
  styx-deploy / styx-oracle / styx-wallet / styx-keeper as the role requires. Or run
  everything as containers: the flake builds reproducible per-role images (same rev, same
  layers) and `deploy/docker/` carries the per-host compose files - see
  [deploy/docker/README.md](deploy/docker/README.md).
- A common chain config. Start from `deploy/producer.elements.conf` (machine A) and
  `deploy/follower.elements.conf` (everyone else). The consensus block at the top must be
  byte-identical everywhere: `chain`, `initialfreecoins`, `evbparams`, `pchmessagestart`.
  A mismatch in `evbparams` does not change the genesis hash - it forks the chain at the
  first Simplicity spend, which is the worst possible failure mode. Diff the files.
- Network options (`port`, `rpcport`, `connect`, `bind`) go under the `[styxnet]` section;
  outside it elementsd silently ignores them.
- Followers set `anyonecanspendaremine=0` (already in the follower template) and point
  `connect=` at machine A.

Firewall: elementsd P2P (18886) open between the machines, RPC (18884) loopback only, the
relay port (7877) open to all machines. The oracle admin ports (9700) open to exactly two
parties: the infra machine (the central monitor walks every oracle's /health, and scenario
control drives the price from one place) and the operator. The admin surface signs quotes
on demand and moves the price - an allowlist, never public.

## 1. Machine A: chain, relay, ceremony

1. Generate RPC credentials (`share/rpcauth/rpcauth.py styx`), paste the `rpcauth=` line
   into the producer config, start the node:

   ```bash
   elementsd -datadir=/var/lib/styxnet   # deploy/systemd/elementsd.service
   ```

2. Start the relay with `deploy/nostr-relay.toml.example` adjusted for the host:

   ```bash
   nostr-rs-relay --config /etc/styx/nostr-relay.toml   # deploy/systemd/nostr-rs-relay.service
   ```

3. Collect the oracle identities. On each oracle machine run `styx-oracle --keygen`, keep
   the secrets there, and send the two printed public keys to the operator. Fill
   `deploy/styxnet.skeleton.toml`: five oracle entries (slot, protocol_pk, nostr_pk,
   admin_url) and the relay URL.

4. Start block production (machine A only; it mines to its own `producer` wallet):

   ```bash
   produce-blocks.sh /var/lib/styxnet 10   # deploy/systemd/styx-produce-blocks.service
   ```

5. Run the ceremony and verify it. The ceremony broadcasts and WAITS for the producer's
   blocks to confirm each step - the same path a public-testnet deployment takes, where
   the federation makes the blocks (a lone node with nothing producing passes
   `--self-mine` instead):

   ```bash
   styx-deploy --rpc-url http://127.0.0.1:18884 --rpc-user styx --rpc-password ... \
       --config styxnet.toml run --reserve-seed 18000000
   styx-deploy ... --config styxnet.toml verify
   ```

   `run` executes the two issuances and seeds the reserve, completing styxnet.toml in
   place; `verify` recompiles the artifacts from the completed file and locates the
   protocol singletons on chain. Distribute the completed `styxnet.toml` to every machine.

## 2. Machines B1..B5: the oracles

Each oracle machine runs a follower node and one daemon:

1. Follower config from the template, `connect=<machine A>` under `[styxnet]`. Start
   elementsd and wait until `getblockhash 0` equals machine A's - if it does not, stop:
   the consensus block differs.
2. Fill `deploy/oracle.toml.example`: the slot, the two secrets from `--keygen`, the relay
   URL, the local RPC credentials.
3. Start the daemon (`deploy/systemd/styx-oracle.service`):

   ```bash
   styx-oracle --config /etc/styx/oracle.toml
   ```

   It signs the configured price at every new block and publishes to the relay. Check:
   `curl http://127.0.0.1:9700/health` shows the advancing height.

## 3. Machine C: the user wallet

1. Follower node as above; genesis check.
2. `styx-wallet keygen`, fill `deploy/wallet.toml.example`.
3. Bootstrap L-BTC. A follower's node wallet is empty by design (the free coins live on
   machine A, and followers run `anyonecanspendaremine=0`), so the coins come from the
   operator: print the funding address here, send from machine A's deploy wallet, and the
   coins land after the next block.

   ```bash
   styx-wallet --config wallet.toml address
   # on machine A (amount in BTC units):
   elements-cli -datadir=/var/lib/styxnet -rpcwallet=styx-deploy \
       sendtoaddress <funding address> 2.0
   ```

   (`styx-wallet fund` also exists, but it draws from the LOCAL node wallet - useful only
   where that wallet actually holds coins, i.e. machine A or a one-host setup.)
4. Use:

   ```bash
   styx-wallet --config wallet.toml open --principal 4000000 --collateral 100000000
   styx-wallet --config wallet.toml status
   ```

   Ticks come from the relay (a 3-of-5 quorum near the tip is required); every command
   syncs the indexer snapshot before acting. `open` sizes the exact funding coin itself.

## 4. Machine D: the keeper

1. Follower node; genesis check.
2. `styx-wallet keygen` for the purse keys, fill `deploy/keeper.toml.example`.
3. Fund the purse: L-BTC for fees from machine A (same bootstrap as the wallet's), OBOL
   for liquidations from any wallet. The keeper config is wallet-shaped, so the wallet CLI
   reads it:

   ```bash
   styx-wallet --config keeper.toml address
   # on machine A: elements-cli ... sendtoaddress <purse address> 0.1
   # from the user wallet: styx-wallet --config wallet.toml send --to <purse address> --amount N
   ```

4. Start the daemon (`deploy/systemd/styx-keeper.service`):

   ```bash
   styx-keeper --config /etc/styx/keeper.toml
   ```

   It pokes the mint anchor toward the tip, refreshes dormant healthy vaults, and walks
   the liquidation ladder when the price moves. One action per block by design; a
   `Rejected` broadcast stops the daemon deliberately - it means a builder invariant
   broke, and that wants eyes, not retries.

## 5. Scenario control

All five oracles follow their configured price until moved:

```bash
deploy/scenario-crash.sh styxnet.toml 50000   # the partial band: the keeper heals
deploy/scenario-crash.sh styxnet.toml 35000   # under water: bad-debt closes the vault
```

Watch the cascade in the keeper journal and in `styx-wallet status`. The reserve pays the
keeper the shortfall cover on a bad debt; the pot returns to the full supply once every
debt is burned.

## Deploying on the public Liquid testnet

The same bring-up with three differences, all already encoded in the tooling:

- No producer and no `--self-mine`: the federation makes the blocks (about one a minute),
  and the ceremony awaits them. Node config is `deploy/liquidtestnet.elements.conf` -
  nothing in it is consensus, the public chain's params are built in. Confirm Simplicity
  before deploying: `elements-cli getdeploymentinfo`.
- The infra wallet's tL-BTC comes from a faucet, to an UNBLINDED address (the deploy
  wallet's coins must be explicit for the raw ceremony paths): create the wallet, get an
  address, `getaddressinfo` it and use the `unconfidential` form with the faucet. Fund it
  well: the ceremony pays the issuances, the reserve seed, and every user bootstrap later.
- The relay and the explorer go behind TLS. `deploy/Caddyfile.example` terminates
  `wss://relay.testnet.styx.network` (proxying nostr-rs-relay on loopback) and
  `https://explorer.testnet.styx.network` (proxying the explorer, and publishing the
  completed config at `/config`); the published config then lists the wss URL.

Skeleton: `deploy/liquid-testnet.skeleton.toml` (chain = liquidtestnet). Oracles run one
exchange each (`[feed]` in oracle.toml with `max_age_secs` armed - a quiet feed must
degrade the quorum, not freeze the price). After `styx-deploy run` + `verify`, the
completed liquid-testnet.toml IS the artifact users download - publish it somewhere
immutable-ish and reference it from TESTNET.md.

Monitoring: `deploy/check-health.sh <config>` is one pass over every oracle's /health
(publishing, feed freshness, heights in lockstep) and the relay; wire it as
`deploy/systemd/styx-monitor.{service,timer}` and hook OnFailure= to a notifier. Before
inviting anyone, run the network-tier feed validation on each oracle host
(`cargo test -p styx-oracle --test live_feeds -- --ignored`) and a soak (below).

## The one-host rehearsal

Inside `nix develop`:

```bash
deploy/swarm.sh [workdir]
```

launches the whole topology as local processes (chain, relay, 5 oracles, wallet, keeper),
opens a vault, hands the principal to the keeper, and drives the crash cascade end to
end. It exits 0 only after the bad-debt closure and the pot back at the full supply. Logs
land in the workdir, one file per process.

The soak variant runs the same topology on LIVE exchange feeds: the system idles on real
market data under the health check for the given duration, then the cascade runs at
prices derived from the live one and the oracles return to the market:

```bash
deploy/soak.sh 3600 [workdir]     # SOAK_BACKENDS overrides the exchange list
```
