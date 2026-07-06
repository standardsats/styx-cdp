# STYX v1 operator guide

The whole protocol by hand on one host: chain, relay, oracles, deployment, a vault and
every owner op on it, the keeper, and a price-crash liquidation. Every command below is
what `deploy/swarm.sh` runs unattended - run that first if you just want to see the system
move; walk this guide when you want to drive it yourself. Multi-machine bring-up is
[SETUP.md](SETUP.md); the protocol itself is specified in `spec-site/`.

Everything happens inside the dev shell:

```bash
nix develop
cargo build --workspace
export BIN=$PWD/target/debug
export WORK=~/styx-playground && mkdir -p $WORK
```

## Units and bands, before anything else

- OBOL units are debt cents: `--principal 4000000` mints $40,000 of OBOL.
- Collateral and fees are L-BTC satoshis: `--collateral 100000000` locks 1 BTC.
- Prices are integer USD per BTC.
- A tick is a 3-of-5 oracle quorum. Mints (open, draw) price at the tick's MIN quote,
  liquidations and redemptions at the MAX quote: both directions are conservative.

Collateral-ratio bands, judged at the max quote:

```text
CR >= 150%          open / draw allowed (the mint gate)
CR >= 130%          healthy; refresh passes
115% < CR < 130%    partial liquidation: a keeper repays dd, extraction capped at
                    1.15 x dd, the vault heals into [132%, 137%]
100% <= CR <= 115%  full liquidation: the keeper repays the whole debt, seizes the
                    collateral, a third of the excess goes to the reserve
CR < 100%           bad debt: the keeper repays the debt, the reserve covers the
                    shortfall plus a bounty (capped), the vault closes
```

- The borrow fee is 0.5% of debt-at-min-quote, paid to the reserve at open and priced into
  the exact funding the wallet shapes automatically.
- Redemption swaps OBOL for collateral at the max quote, floored by the backing ratio,
  0.5% fee to the reserve. Permissionless, no health gate.
- Every vault op that carries a tick needs it strictly newer than the vault's last
  acknowledged height: consecutive ops on one vault want a block between them.

## 1. The chain

One node, producer role (on a real network the followers differ only in config - see
SETUP.md):

```bash
mkdir -p $WORK/node
cat > $WORK/node/elements.conf <<EOF
chain=styxnet
initialfreecoins=210000000000
evbparams=simplicity:-1:::
validatepegin=0
fallbackfee=0.0001
blindedaddresses=0
txindex=1
rpcuser=styx
rpcpassword=styx
pchmessagestart=53545958

[styxnet]
port=7042
rpcport=7041
listen=0
rpcbind=127.0.0.1
rpcallowip=127.0.0.1
EOF
elementsd -datadir=$WORK/node &
elements-cli -datadir=$WORK/node getblockcount   # 0 when up
```

Start block production right away (it mines to its own `producer` wallet; 5s is a
comfortable pace for driving by hand). Everything downstream - the ceremony included -
waits for these blocks:

```bash
ELEMENTS_CLI=elements-cli deploy/produce-blocks.sh $WORK/node 5 &
```

## 2. The relay

```bash
mkdir -p $WORK/relay-db
cat > $WORK/relay.toml <<EOF
[info]
relay_url = "ws://127.0.0.1:7877"
name = "styxnet-playground"

[database]
data_directory = "$WORK/relay-db"

[network]
address = "127.0.0.1"
port = 7877
EOF
nostr-rs-relay --config $WORK/relay.toml &
```

## 3. Oracle identities and the shared config

Each oracle owns two keys: the protocol key (what the covenants verify quotes against)
and a Nostr key (transport identity only). Generate five identities:

```bash
for i in 0 1 2 3 4; do $BIN/styx-oracle --keygen > $WORK/oracle-$i.keys; done
```

Assemble the skeleton of `styxnet.toml` - the one artifact every role shares. Public keys
from the `.keys` files; the admin URLs are where scenario control will POST prices:

```toml
# $WORK/styxnet.toml
[network]
chain = "styxnet"

[[oracles]]
slot = 0
protocol_pk = "<protocol_pk from oracle-0.keys>"
nostr_pk = "<nostr_pk from oracle-0.keys>"
admin_url = "http://127.0.0.1:9700"

# ... slots 1..4, admin ports 9701..9704 ...

[nostr]
relays = ["ws://127.0.0.1:7877"]
```

## 4. The deployment ceremony

One command issues the fixed OBOL supply into the pot, the 1-unit issuer identity token,
and seeds the stability reserve; it completes `styxnet.toml` in place with the genesis,
the asset ids, and the issuer anchor. It waits for the producer's blocks to confirm each
step - the exact path a testnet deployment takes, where the federation makes the blocks
(a lone node with nothing producing would pass `--self-mine` instead):

```bash
$BIN/styx-deploy --rpc-url http://127.0.0.1:7041 --rpc-user styx --rpc-password styx \
    --config $WORK/styxnet.toml run --reserve-seed 18000000
```

`verify` is the standing check that the compiled covenants match what is on chain - run it
whenever in doubt:

```bash
$BIN/styx-deploy --rpc-url http://127.0.0.1:7041 --rpc-user styx --rpc-password styx \
    --config $WORK/styxnet.toml verify
```

## 5. The oracle daemons

One config per slot; the secrets come from the matching `.keys` file:

```toml
# $WORK/oracle-0.toml
slot = 0
protocol_seckey = "<from oracle-0.keys>"
nostr_seckey = "<from oracle-0.keys>"
relays = ["ws://127.0.0.1:7877"]
rpc_url = "http://127.0.0.1:7041"
rpc_user = "styx"
rpc_password = "styx"
listen = "127.0.0.1:9700"
price_usd = 120000
poll_ms = 500
```

```bash
for i in 0 1 2 3 4; do
  $BIN/styx-oracle --config $WORK/oracle-$i.toml > $WORK/oracle-$i.log 2>&1 &
done
```

Each daemon signs its configured price at every new block and publishes one addressable
Nostr event per height. Poke the surfaces:

```bash
curl http://127.0.0.1:9700/health            # {"slot":0,"height":...,"price":120000}
curl "http://127.0.0.1:9700/quote?height=42" # a freshly signed quote, the backup channel
curl -X POST -H 'content-type: application/json' -d '{"usd":115000}' \
    http://127.0.0.1:9700/price              # move ONE oracle's price
```

A single moved oracle only drags the quorum's min (or max) if it makes the assembled
three; crashing the market for real means moving all of them - that is what
`deploy/scenario-crash.sh` does, later.

## 6. The wallet

```bash
$BIN/styx-wallet keygen > $WORK/wallet.keys
```

```toml
# $WORK/wallet.toml
styxnet = "$WORK/styxnet.toml"          # absolute path
rpc_url = "http://127.0.0.1:7041/wallet/styx-deploy"
rpc_user = "styx"
rpc_password = "styx"
snapshot = "$WORK/wallet-snapshot.json" # absolute path
owner_seckey = "<from wallet.keys>"
funding_seckey = "<from wallet.keys>"
```

Two keys, two jobs: the funding key is one key-path p2tr script holding every coin (L-BTC
and OBOL alike); the owner key signs owner ops and never appears on chain as an address -
only inside vault commitments. The wallet's vaults are whatever its indexer resolves to
that key.

Fund it. On this host the node wallet holds the free coins, so `fund` works directly (on
a follower machine the coins come from the operator - SETUP.md):

```bash
$BIN/styx-wallet --config $WORK/wallet.toml fund --sats 200000000
# broadcast only: the coin shows in `status` once the producer's next block lands
$BIN/styx-wallet --config $WORK/wallet.toml status
# L-BTC: 200000000 sats in 1 coins | OBOL: 0 units in 0 coins
```

Every command syncs the indexer from the snapshot before acting; `status` also shows the
protocol state (pot, reserve, issuer anchor) and your vaults.

## 7. Open a vault

$40k of OBOL against 1 BTC at $120k is a 300% CR - comfortably above the 150% gate:

```bash
$BIN/styx-wallet --config $WORK/wallet.toml open --principal 4000000 --collateral 100000000
```

Or let the wallet size the collateral for a target CR: `--cr 200` instead of
`--collateral`. Under the hood the wallet assembles a tick from the relay, shapes an
exact funding coin (collateral + 0.5% borrow fee + tx fee - the frozen OPEN layout has no
change output) with a self-spend, and chains the open on it in the same mempool. After
the next block:

```bash
$BIN/styx-wallet --config $WORK/wallet.toml status
# vaults: 1
#   <outpoint> debt 4000000 coll 100000000 last_height <h>
# L-BTC: ~99.8M sats | OBOL: 4000000 units
```

The principal landed at your own funding script as an OBOL coin. The reserve grew by the
borrow fee; the pot shrank by the principal.

## 8. Owner ops

One block between tick-carrying ops on the same vault (the freshness ratchet is strict).

```bash
# repay $5k of debt; the surplus of the paying coin returns as change
$BIN/styx-wallet --config $WORK/wallet.toml repay --amount 500000

# draw $5k more against the same collateral (re-checks the 150% gate at the min quote)
$BIN/styx-wallet --config $WORK/wallet.toml draw --amount 500000

# advance the freshness ratchet without touching balances (proves CR >= 130%)
$BIN/styx-wallet --config $WORK/wallet.toml refresh

# redeem $2k of your own OBOL against the vault at the max quote (permissionless op:
# any OBOL holder could do this to any vault; the wallet targets its own)
$BIN/styx-wallet --config $WORK/wallet.toml redeem --x 200000

# repay the whole remaining debt, free the collateral back to the funding script
$BIN/styx-wallet --config $WORK/wallet.toml close

# move coins between roles (OBOL by default, --lbtc for sats)
$BIN/styx-wallet --config $WORK/wallet.toml send --to <address> --amount 1000000
```

With several vaults, target one with `--vault txid:vout` (from `status`); with exactly
one it is picked automatically. Typed refusals name the covenant gate they mirror: a
stale tick, an undercollateralized draw, a repay above the debt.

## 9. The keeper

The keeper is a wallet-shaped purse plus a watchtower loop, so the wallet CLI reads its
config for funding and status:

```bash
$BIN/styx-wallet keygen > $WORK/keeper.keys
```

```toml
# $WORK/keeper.toml: the same fields as wallet.toml (own keys, own snapshot path), plus:
poke_lag = 2
refresh_lag = 6
poll_ms = 1000
```

Fund the purse: L-BTC for fees, OBOL for liquidations (a keeper repays debt with its own
OBOL and is compensated in collateral):

```bash
$BIN/styx-wallet --config $WORK/keeper.toml fund --sats 5000000
$BIN/styx-wallet --config $WORK/keeper.toml address
$BIN/styx-wallet --config $WORK/wallet.toml send --to <keeper funding address> --amount 4000000
```

Open a fresh vault to give it something to watch (section 7), then start it:

```bash
$BIN/styx-keeper --config $WORK/keeper.toml > $WORK/keeper.log 2>&1 &
tail -f $WORK/keeper.log
```

At a healthy price you will see the duties: `Poked` whenever the issuer's mint anchor
lags the tip by more than `poke_lag` blocks (the anchor is the freshness floor every mint
feeds on), and `Refreshed` for any healthy vault whose ratchet lags by more than
`refresh_lag` - including yours, and including vaults of complete strangers: the keeper
recovers vault owners from the OPEN transaction's witness, so every vault born on chain
is a full target. One action per block by design.

## 10. The crash

Drop every oracle to $50k. With 4M debt and 100M collateral that is CR 125% - the partial
band:

```bash
deploy/scenario-crash.sh $WORK/styxnet.toml 50000
```

Within a few blocks the keeper log shows:

```text
performed: Partial { vault: ..., dd: Obol(2181818), ... }
```

Read it against the bands: the keeper repaid $21,818.18 of debt (the largest dd whose
extraction fits the 1.15 x dd cap), took collateral worth at most 115% of that, and the
vault healed to ~132-137% CR at the new price. `status` on the wallet side shows the
smaller debt and collateral; the reserve grew by its 5% share of dd.

Now drop to $35k - the healed vault falls under water (CR ~93%):

```bash
deploy/scenario-crash.sh $WORK/styxnet.toml 35000
# keeper log: performed: BadDebt { vault: ..., ... }
```

The keeper repaid the remaining debt, seized the collateral, and the reserve covered the
shortfall plus the bounty (capped at 20% of debt and by its balance). The vault is gone;
`status` shows `vaults: 0`, the pot back at the full supply (all OBOL burned home), the
reserve visibly smaller, and the keeper's L-BTC noticeably larger than it was funded
with - that is the liquidation premium doing its job.

Recovery is just the mirror: raise the price back and open again.

## 11. Reading the system

```bash
# wallet or keeper view (protocol state + own coins + own vaults):
$BIN/styx-wallet --config <role>.toml status

# chain level:
elements-cli -datadir=$WORK/node getblockcount
elements-cli -datadir=$WORK/node getrawmempool

# oracles: curl /health per slot; the relay stores one event per (oracle, height)
```

Keeper log verbs: `Poked` / `Refreshed` / `Partial` / `FullLiq` / `BadDebt` /
`ResolvedElsewhere` (lost a race to another keeper - normal in a fleet).

Troubleshooting:

- "no oracle quorum within 8 blocks of the tip": oracles not publishing (check their
  logs and /health) or the relay is down.
- "genesis mismatch": the config's chain and the node's chain differ - wrong datadir or a
  forked consensus block; nothing was signed.
- "Transaction already in block chain" in daemon logs is benign: broadcast is idempotent
  and daemons poll faster than blocks confirm.
- The keeper exiting with code 65 is deliberate: the node rejected what the builders
  produced, which means an invariant broke - it wants investigation, not a restart (the
  systemd unit encodes exactly that).

## 12. Teardown

```bash
kill %1 %2 %3 ...        # or the pids you noted; everything lives under $WORK
rm -rf $WORK
```

State worth knowing about: indexer snapshots (`*-snapshot.json`, safe to delete - they
rebuild from genesis), the node datadir, the relay database. Secrets live inline in the
role configs - wipe them with the workdir.
