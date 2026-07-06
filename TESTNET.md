# STYX on Liquid testnet

Open vaults, mint OBOL, and liquidate other people's positions with real market prices -
on the public Liquid testnet, with worthless coins. Everything is permissionless: the
protocol is five frozen covenants and there is no admin key anywhere; the oracles and the
quote relay are the only operated pieces.

> **Status: the public deployment is not live yet.** The endpoints below
> (`explorer.testnet.styx.network`, `relay.testnet.styx.network`,
> `oracle{0..4}.testnet.styx.network`) are where it will run. Until then, stand the whole
> thing up on one machine with `deploy/swarm.sh` (see [GUIDE.md](GUIDE.md)) - the flow is
> identical, only the config points at localhost.

Watch the protocol live first - the explorer at **https://explorer.testnet.styx.network**
shows every vault, the CR bands, oracle freshness, and the liquidation feed, straight from
the chain, no install.

Two ways to act: the **app** (a local web UI for the wallet and the keeper - the easy path)
or the **CLI** (scriptable, same library underneath). Both talk only to your own node and
the public relay; there is no trusted server. Either way you need the binaries, a
Liquid-testnet node, and the published config.

## 1. Binaries and node

```bash
git clone https://github.com/styx-network/cdp-styx-v1 && cd cdp-styx-v1
nix develop            # rustc, cargo, and the Simplicity-capable elementsd
cargo build --release
export BIN=$PWD/target/release
```

Run your own node (every role talks only to its own node; there is no trusted server):

```bash
mkdir -p ~/.styx/node
cp deploy/liquidtestnet.elements.conf ~/.styx/node/elements.conf
# generate rpcauth per the comment in the file, then:
elementsd -datadir=~/.styx/node
```

Wait for sync, then confirm Simplicity is live on this chain:

```bash
elements-cli -datadir=~/.styx/node getdeploymentinfo | grep -A3 simplicity
```

## 2. The deployment config

Download the published `liquid-testnet.toml` from
**https://explorer.testnet.styx.network/config** - it carries the genesis, the asset ids,
the five oracle keys, and the relay address (`wss://relay.testnet.styx.network`). Your
wallet cross-checks the genesis against your node before signing anything, so a wrong or
tampered file refuses to act rather than misbehave.

## 3. Keys and the funding drop

One keypair backs everything - the wallet, the app, and the keeper all read the same two
secrets from their config.

```bash
$BIN/styx-wallet keygen > ~/.styx/wallet.keys   # owner_seckey + funding_seckey
```

Get tL-BTC. Print your funding address and point a Liquid-testnet faucet (for example the
one at liquidtestnet.com) DIRECTLY at it:

```bash
# write ~/.styx/wallet.toml first (step 4a), then:
$BIN/styx-wallet --config ~/.styx/wallet.toml address
# funding address: tex1p...   <- give this to the faucet
```

The funding address is deliberately unblinded, so faucet coins arrive spendable by the
protocol tooling as-is. Do not route them through your node wallet first: its addresses
are confidential and the raw funding paths cannot spend confidential coins. First sync
starts at the deployment height, not genesis - seconds, not hours.

## 4. The app (the easy path)

The app is the wallet and the keeper behind one local page:

```bash
cp deploy/app.toml.example ~/.styx/app.toml   # edit: paths, rpc password, the two keys
$BIN/styx-app --config ~/.styx/app.toml
# styx-app up: open http://127.0.0.1:9780/ in your browser
```

The page shows your address and balances, the vault table with CR bands, forms for every
op (open sized from the live price by a CR slider, repay / draw / refresh / redeem /
close), and a keeper toggle that runs the hunt on these same coins. The UI is served by
your own binary on loopback with a per-session token printed at startup, so keys never
leave your machine. The vault owner key can even stay off the box entirely - the app can
export a close, repay, or draw for an external signer to sign off-machine (the "External
signer" card; more in `PACKAGING.md`).

If the app is all you want, skip to section 6 (what the keeper does) and section 7
(troubleshooting). The CLI below does the same things, scriptably.

## 5. The CLI

### 5a. wallet.toml

```toml
# ~/.styx/wallet.toml (absolute paths)
styxnet = "/home/you/.styx/liquid-testnet.toml"
rpc_url = "http://127.0.0.1:18884"
rpc_user = "styx"
rpc_password = "<your rpcauth password>"
snapshot = "/home/you/.styx/wallet-snapshot.json"
owner_seckey = "<from wallet.keys>"
funding_seckey = "<from wallet.keys>"
```

```bash
$BIN/styx-wallet --config ~/.styx/wallet.toml status
# L-BTC: <faucet amount> sats in 1 coins | OBOL: 0 units in 0 coins
```

### 5b. Vaults

Units: OBOL units are debt cents (`--principal 100000` = $1,000), collateral is satoshis,
prices are USD per BTC from a live 3-of-5 oracle quorum. Mints price at the quorum's MIN
quote, liquidations and redemptions at the MAX.

```bash
# $1,000 of OBOL at 200% CR - the wallet sizes the collateral from the live price
$BIN/styx-wallet --config ~/.styx/wallet.toml open --principal 100000 --cr 200

$BIN/styx-wallet --config ~/.styx/wallet.toml status
$BIN/styx-wallet --config ~/.styx/wallet.toml repay --amount 20000
$BIN/styx-wallet --config ~/.styx/wallet.toml draw --amount 10000
$BIN/styx-wallet --config ~/.styx/wallet.toml refresh
$BIN/styx-wallet --config ~/.styx/wallet.toml redeem --x 10000
$BIN/styx-wallet --config ~/.styx/wallet.toml close
$BIN/styx-wallet --config ~/.styx/wallet.toml send --to <address> --amount 50000
```

Blocks arrive about once a minute from the federation: after any op, the next block makes
it visible to `status`. Ops that carry an oracle tick (open, draw, refresh, redeem) need
one strictly newer than the vault's last - consecutive ops on one vault want a block
between them. The issuer singleton serializes every open and draw on the whole chain; the
wallet retries contention automatically, but under load expect an occasional
"still conflicted, retry".

Health bands, judged at the MAX quote:

```text
CR >= 150%          open / draw pass
CR >= 130%          healthy
115% < CR < 130%    partial liquidation territory
100% <= CR <= 115%  full liquidation
CR < 100%           bad debt (the reserve covers the shortfall)
```

## 6. Hunting: run a keeper

The keeper liquidates ANY unhealthy vault - it recovers vault owners from the chain
itself, no registration anywhere. It needs its own keys, tL-BTC for fees, and OBOL to
repay other people's debt (open a vault and keep the principal, or buy it off someone):

```bash
$BIN/styx-wallet keygen > ~/.styx/keeper.keys
# ~/.styx/keeper.toml: same fields as wallet.toml (own keys, own snapshot), plus:
#   poke_lag = 4
#   refresh_lag = 16
#   poll_ms = 5000

$BIN/styx-wallet --config ~/.styx/keeper.toml address   # fund this with tL-BTC + OBOL
$BIN/styx-keeper --config ~/.styx/keeper.toml
```

The log shows its verbs: `Poked` and `Refreshed` are watchtower duties (paid in nothing
but karma), `Partial` / `FullLiq` / `BadDebt` are the paid work - the keeper spends OBOL
to repay the debt and takes collateral worth more (up to 115% of what it repaid, plus the
reserve's cover on bad debt). `ResolvedElsewhere` means another keeper beat you to it:
prices come from five live exchanges, so real dips make real races.

## 7. When something looks wrong

- `no oracle quorum near the tip`: the relay or the oracles are behind. The explorer at
  https://explorer.testnet.styx.network shows per-slot oracle freshness - if the slots are
  stale there too, it is the deployment, not you; wait or report it.
- `genesis mismatch`: your node is on the wrong chain, or the config file is not the
  published one. Nothing was signed.
- The keeper exiting with code 65: the node rejected what the builders produced. That is
  a protocol-level bug worth reporting, not a restart.
- Wallet coins missing after a faucet drop: wait a block; `status` only counts confirmed
  coins.

The full op-by-op tour (with the exact economics of each band) is [GUIDE.md](GUIDE.md);
the protocol specification is `spec-site/`.
