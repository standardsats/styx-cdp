# STYX testnet deployment runbook

The ordered deploy-day sequence for the public Liquid-testnet deployment at
`*.testnet.styx.network`. This stitches the reference material into one path: the multi-
machine background is [SETUP.md](SETUP.md), the container images are `deploy/docker/`, the
protocol tour is [GUIDE.md](GUIDE.md). Rehearse it first with `deploy/swarm.sh` (one host,
localhost config) and a `deploy/soak.sh` run - both must be green before touching the real
domains.

The public chain is `liquidtestnet` (the federation makes the blocks), so the ceremony runs
in await-confirmation mode: no `--self-mine` anywhere.

## Topology and DNS

```
infra host   relay + Caddy (TLS) + explorer + monitor + the deploy node
oracle 0..4  a follower node + one styx-oracle each, one exchange per slot
```

DNS records to create on styx.network before anything else (A / AAAA to the hosts):

```
relay.testnet.styx.network       -> infra host
explorer.testnet.styx.network    -> infra host
oracle0.testnet.styx.network     -> oracle host 0
...                              ...
oracle4.testnet.styx.network     -> oracle host 4
```

The oracle hostnames are for your own operational addressing; users never contact the
oracles directly (quotes flow over the relay), and the oracle admin ports stay private.

## 0. Prerequisites, every host

- The Simplicity-capable elementsd and the role binaries: either `nix build .#styx` and the
  per-role container images (`deploy/docker/README.md`, tag = flake rev), or
  `cargo build --release` from a pinned checkout. Same rev everywhere.
- A Liquid-testnet node config from `deploy/liquidtestnet.elements.conf` - generate the
  `rpcauth` line with `share/rpcauth/rpcauth.py styx`, keep RPC loopback-only.
- Firewall: node P2P open as the federation needs; node RPC (18884) loopback only; the relay
  port behind Caddy on 443; oracle admin ports (9700) reachable only from the infra host (for
  the health monitor) and from wherever you run scenario overrides - never public.

## 1. Infra host

1. Start the node, wait for sync, confirm Simplicity:

   ```bash
   elementsd -datadir=/var/lib/styxnet    # deploy/systemd/elementsd.service
   elements-cli -datadir=/var/lib/styxnet getdeploymentinfo | grep -A3 simplicity
   ```

2. Relay behind TLS. `deploy/nostr-relay.toml.example` on loopback:7877; Caddy from
   `deploy/Caddyfile.example` (already carries `relay.testnet.styx.network` and
   `explorer.testnet.styx.network`) terminates wss/https and fetches the certificates.

   ```bash
   nostr-rs-relay --config /etc/styx/nostr-relay.toml   # styx-produce-blocks is styxnet-only; skip it here
   caddy run --config /etc/styx/Caddyfile
   ```

## 2. Oracle hosts (0..4)

Each oracle machine runs a follower node and one daemon. Do all five, one exchange per slot
(coinbase / binance / kraken / okx / bitfinex - genuinely independent sources).

1. Follower node; wait for sync; confirm `getblockhash 0` matches the federation.
2. Generate the oracle's keys and keep them on the box:

   ```bash
   styx-oracle --keygen > /etc/styx/oracle-<N>.keys      # chmod 600
   ```

   Send the two printed PUBLIC halves (`protocol_pk`, `nostr_pk`) to the operator.
3. Fill `deploy/oracle.toml.example`: `slot = <N>`, the two secrets, `relays =
   ["wss://relay.testnet.styx.network"]`, the local RPC auth, and a `[feed]` with this
   slot's exchange. Arm the staleness gate - this is a production deploy:

   ```toml
   [feed]
   backend = "coinbase"      # a different exchange per slot
   poll_ms = 5000
   max_age_secs = 60         # stop publishing when the feed goes quiet: five gated oracles
                             # freeze the quorum safely, five ungated ones re-sign a stale price
   ```

4. Start it and check the feed is live:

   ```bash
   styx-oracle --config /etc/styx/oracle.toml            # deploy/systemd/styx-oracle.service
   curl http://127.0.0.1:9700/health                     # source "feed", advancing height
   ```

## 3. The ceremony (infra host)

1. Complete `deploy/liquid-testnet.skeleton.toml`: paste the five oracle `protocol_pk` /
   `nostr_pk` collected above, and each oracle's `admin_url` (the infra-reachable address,
   e.g. `http://oracle0.testnet.styx.network:9700` if the monitor reaches them by name).
   The relay line is already `wss://relay.testnet.styx.network`.

2. Fund a deploy wallet from the faucet. Create/load a node wallet, print an address, point
   the Liquid-testnet faucet at it (the ceremony pays the two issuances and the reserve seed
   from this wallet, so give it enough tL-BTC).

3. Preflight, then run and verify - NO `--self-mine` (the federation confirms):

   ```bash
   RPC="--rpc-url http://127.0.0.1:18884 --rpc-user styx --rpc-password <pw>"
   elements-cli -datadir=/var/lib/styxnet getdeploymentinfo | grep -A3 simplicity   # Simplicity active
   styx-deploy $RPC --config liquid-testnet.toml run --reserve-seed 18000000
   styx-deploy $RPC --config liquid-testnet.toml verify
   ```

   `run` fills the config with the genesis, the asset ids, and the issuer anchor; `verify`
   recompiles the artifacts and locates the singletons on chain (the compiled-pins-vs-
   published-artifacts check). If `verify` disagrees, stop - do not publish.

4. Publish the completed `liquid-testnet.toml` at the config URL the docs point users to:

   ```bash
   cp liquid-testnet.toml /var/lib/styx/liquid-testnet.toml   # Caddy serves it at /config
   curl https://explorer.testnet.styx.network/config          # confirm it downloads
   ```

   The same bytes go into the repository as `deploy/liquid-testnet.toml` (signed commit,
   the second source `join-testnet.sh` falls back to), and the values it carries are
   restated in prose on `landing/testnet.html` - genesis, the two asset ids, the issuer
   anchor, the five oracle keys, the relay. A re-deploy changes all three; leaving one
   behind hands users a config that resolves to addresses where nothing lives.

## 4. Explorer and monitor (infra host)

1. Fill `deploy/explorer.toml.example`: its own node RPC, the published
   `liquid-testnet.toml` as `styxnet`, `listen = "127.0.0.1:9790"` (Caddy fronts it), and
   `app_url` = your styx-app download link. Start it; confirm the page renders and shows the
   singletons and the oracle recency:

   ```bash
   styx-explorer --config /etc/styx/explorer.toml
   curl -s https://explorer.testnet.styx.network/api/state | head
   ```

2. Health monitor: install `check-health.sh` and the `styx-monitor` systemd timer against
   the published config, and wire an `OnFailure=` notifier so a silent oracle or a stale
   feed pages you.

   ```bash
   systemctl enable --now styx-monitor.timer      # deploy/systemd/styx-monitor.{service,timer}
   ```

## 5. OBOL for the first keepers

Keepers need OBOL to repay other people's debt. Seed a treasury: open one vault yourself and
distribute its principal with `send`.

```bash
styx-wallet --config ~/.styx/treasury.toml open --principal 5000000 --cr 200   # $50k of OBOL
styx-wallet --config ~/.styx/treasury.toml send --to <keeper funding address> --amount 1000000
```

## 6. Soak, then invite

Before announcing, let it run under the monitor and confirm every oracle host has a live,
non-stale feed for a sustained window (a multi-day soak). The rehearsal form is
`deploy/soak.sh` on one host; on the real deployment the equivalent is the monitor staying
green across all five oracle hosts with `check-health.sh` clean, plus one manual owner cycle
and one keeper liquidation through the live system.

Go-live checklist:

- [ ] `verify` agreed and the config is published at `/config` (downloads over https).
- [ ] All five oracles publishing, distinct exchanges, `max_age_secs` armed; the explorer
      shows five fresh slots.
- [ ] The monitor timer is enabled with a notifier on failure.
- [ ] A treasury vault exists and OBOL has reached at least one keeper.
- [ ] One end-to-end owner cycle (open through close) and one keeper liquidation confirmed
      on the live chain.
- [x] The "not live yet" banner and the URL notes in [TESTNET.md](TESTNET.md) are removed /
      finalized.

Only then announce and point people at TESTNET.md.
