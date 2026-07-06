# STYX on Liquid testnet

Open vaults, mint OBOL, and liquidate other people's positions with real market prices -
on the public Liquid testnet, with worthless coins. Everything is permissionless: the
protocol is five frozen covenants and there is no admin key anywhere; the oracles and the
quote relay are the only operated pieces.

Watch the protocol live first - the explorer at <EXPLORER-URL> shows every vault, the CR
bands, oracle freshness, and the liquidation feed, straight from the chain, no install.

To act, you need three things: the binaries, a Liquid-testnet node, and the deployment
config.

## 1. Binaries and node

```bash
git clone <REPO-URL> && cd cdp-styx-v1
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

Download the published `liquid-testnet.toml` (see <CONFIG-URL>) - it carries the genesis,
the asset ids, the five oracle keys, and the relay address. Your wallet cross-checks the
genesis against your node before signing anything, so a wrong or tampered file refuses to
act rather than misbehave.

## 3. A wallet

```bash
$BIN/styx-wallet keygen > ~/.styx/wallet.keys
```

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

First sync starts at the deployment height, not genesis - seconds, not hours.

Get tL-BTC. Point the faucet (for example the one at liquidtestnet.com) DIRECTLY at your
wallet's funding address:

```bash
$BIN/styx-wallet --config ~/.styx/wallet.toml address
# funding address: tex1p...   <- give this to the faucet
```

The funding address is deliberately unblinded, so faucet coins arrive spendable by the
protocol tooling as-is. Do not route them through your node wallet first: its addresses
are confidential and the raw funding paths cannot spend confidential coins.

```bash
$BIN/styx-wallet --config ~/.styx/wallet.toml status
# L-BTC: <faucet amount> sats in 1 coins | OBOL: 0 units in 0 coins
```

## 4. Vaults

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

## 5. Hunting: run a keeper

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

## 6. When something looks wrong

- `no oracle quorum near the tip`: the relay or the oracles - check
  `curl <admin_url>/health` for `"publishing":true` (the operator's monitoring watches
  the same thing).
- `genesis mismatch`: your node is on the wrong chain, or the config file is not the
  published one. Nothing was signed.
- The keeper exiting with code 65: the node rejected what the builders produced. That is
  a protocol-level bug worth reporting, not a restart.
- Wallet coins missing after a faucet drop: wait a block; `status` only counts confirmed
  coins.

The full op-by-op tour (with the exact economics of each band) is [GUIDE.md](GUIDE.md);
the protocol specification is `spec-site/`.
