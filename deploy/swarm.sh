#!/usr/bin/env bash
# The one-host swarm rehearsal: every role as a real process against a real styxnet chain
# and a real Nostr relay - the dress rehearsal for the multi-machine bring-up in SETUP.md.
#
#   elementsd (producer) + block loop
#   nostr-rs-relay
#   styx-deploy (ceremony)
#   styx-oracle x5 (fresh keys, admin HTTP on 9700..9704)
#   styx-wallet  (fund, open, hand OBOL to the keeper)
#   styx-keeper  (poke/refresh duties, then the crash cascade)
#
# Scenario: all five oracles crash to $50k (partial liquidation heals the vault), then to
# $35k (bad debt closes it). Exits 0 when the cascade completed and the pot is back at the
# full supply.
#
# Run inside `nix develop` (elementsd, elements-cli, nostr-rs-relay, cargo on PATH):
#   deploy/swarm.sh [workdir]
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=${1:-$(mktemp -d /tmp/styx-swarm.XXXXXX)}
BIN="$ROOT/target/debug"
RPC_PORT=7041
P2P_PORT=7042
RELAY_PORT=7877
RPC="http://127.0.0.1:$RPC_PORT"
BLOCK_INTERVAL=2

for tool in elementsd elements-cli nostr-rs-relay cargo curl; do
  command -v "$tool" >/dev/null || { echo "$tool not on PATH (run inside nix develop)"; exit 1; }
done

PIDS=()
cleanup() {
  echo "--- teardown"
  for pid in "${PIDS[@]:-}"; do kill "$pid" 2>/dev/null || true; done
  wait 2>/dev/null || true
}
trap cleanup EXIT

log() { echo "--- $*"; }

# Wait until a pattern shows up in a log file.
wait_log() {
  local pattern=$1 file=$2 timeout=${3:-90}
  for _ in $(seq "$timeout"); do
    grep -q "$pattern" "$file" 2>/dev/null && return 0
    sleep 1
  done
  echo "timed out waiting for '$pattern' in $file" >&2
  tail -5 "$file" >&2 || true
  return 1
}

# Wait until the wallet status of a role shows a pattern (coins confirm, vaults appear).
# Capture-then-grep: grep -q on a live pipe exits early, and under pipefail the resulting
# SIGPIPE would read as a miss even when the pattern matched.
wait_status() {
  local config=$1 pattern=$2 timeout=${3:-60} out
  for _ in $(seq "$timeout"); do
    out=$("$BIN/styx-wallet" --config "$config" status 2>/dev/null || true)
    if grep -q "$pattern" <<< "$out"; then
      return 0
    fi
    sleep 1
  done
  echo "timed out waiting for '$pattern' in $config status" >&2
  "$BIN/styx-wallet" --config "$config" status >&2 || true
  return 1
}

log "building the workspace"
(cd "$ROOT" && cargo build --workspace --quiet)

log "workdir $WORK"
mkdir -p "$WORK/node" "$WORK/relay-db"

# --- 1. the chain: one styxnet node, producing blocks -------------------------------
cat > "$WORK/node/elements.conf" <<EOF
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
port=$P2P_PORT
rpcport=$RPC_PORT
listen=0
rpcbind=127.0.0.1
rpcallowip=127.0.0.1
EOF
elementsd -datadir="$WORK/node" > "$WORK/elementsd.log" 2>&1 &
PIDS+=($!)
for _ in $(seq 30); do
  elements-cli -datadir="$WORK/node" getblockcount >/dev/null 2>&1 && break
  sleep 1
done
log "elementsd up at height $(elements-cli -datadir="$WORK/node" getblockcount)"

# --- 2. the relay --------------------------------------------------------------------
cat > "$WORK/relay.toml" <<EOF
[info]
relay_url = "ws://127.0.0.1:$RELAY_PORT"
name = "styxnet-swarm"

[database]
data_directory = "$WORK/relay-db"

[network]
address = "127.0.0.1"
port = $RELAY_PORT
EOF
nostr-rs-relay --config "$WORK/relay.toml" > "$WORK/relay.log" 2>&1 &
PIDS+=($!)

# --- 3. oracle identities + the shared skeleton --------------------------------------
{
  echo '[network]'
  echo 'chain = "styxnet"'
  echo ''
} > "$WORK/styxnet.toml"
for i in 0 1 2 3 4; do
  "$BIN/styx-oracle" --keygen > "$WORK/oracle-$i.keys"
  P_SK=$(sed -n 's/^protocol_seckey = "\(.*\)"/\1/p' "$WORK/oracle-$i.keys")
  N_SK=$(sed -n 's/^nostr_seckey = "\(.*\)"/\1/p' "$WORK/oracle-$i.keys")
  P_PK=$(sed -n 's/^protocol_pk = "\(.*\)"/\1/p' "$WORK/oracle-$i.keys")
  N_PK=$(sed -n 's/^nostr_pk = "\(.*\)"/\1/p' "$WORK/oracle-$i.keys")
  {
    echo '[[oracles]]'
    echo "slot = $i"
    echo "protocol_pk = \"$P_PK\""
    echo "nostr_pk = \"$N_PK\""
    echo "admin_url = \"http://127.0.0.1:970$i\""
    echo ''
  } >> "$WORK/styxnet.toml"
  cat > "$WORK/oracle-$i.toml" <<EOF
slot = $i
protocol_seckey = "$P_SK"
nostr_seckey = "$N_SK"
relays = ["ws://127.0.0.1:$RELAY_PORT"]
rpc_url = "$RPC"
rpc_user = "styx"
rpc_password = "styx"
listen = "127.0.0.1:970$i"
price_usd = 120000
poll_ms = 500
EOF
done
{
  echo '[nostr]'
  echo "relays = [\"ws://127.0.0.1:$RELAY_PORT\"]"
} >> "$WORK/styxnet.toml"

# --- 4. the ceremony ------------------------------------------------------------------
log "deploying the protocol"
"$BIN/styx-deploy" --rpc-url "$RPC" --rpc-user styx --rpc-password styx \
  --config "$WORK/styxnet.toml" run --reserve-seed 18000000
"$BIN/styx-deploy" --rpc-url "$RPC" --rpc-user styx --rpc-password styx \
  --config "$WORK/styxnet.toml" verify

# Block production starts after the ceremony created the node wallet it mines to.
ELEMENTS_CLI=elements-cli "$ROOT/deploy/produce-blocks.sh" "$WORK/node" "$BLOCK_INTERVAL" \
  > "$WORK/producer.log" 2>&1 &
PIDS+=($!)

# --- 5. the oracles --------------------------------------------------------------------
for i in 0 1 2 3 4; do
  "$BIN/styx-oracle" --config "$WORK/oracle-$i.toml" > "$WORK/oracle-$i.log" 2>&1 &
  PIDS+=($!)
done
wait_log "quote published" "$WORK/oracle-0.log" 30
log "oracles publishing"

# --- 6. the roles: wallet and keeper ----------------------------------------------------
for role in wallet keeper; do
  "$BIN/styx-wallet" keygen > "$WORK/$role.keys"
  OWNER=$(sed -n 's/^owner_seckey = "\(.*\)"/\1/p' "$WORK/$role.keys")
  FUNDING=$(sed -n 's/^funding_seckey = "\(.*\)"/\1/p' "$WORK/$role.keys")
  cat > "$WORK/$role.toml" <<EOF
styxnet = "$WORK/styxnet.toml"
rpc_url = "$RPC/wallet/styx-deploy"
rpc_user = "styx"
rpc_password = "styx"
snapshot = "$WORK/$role-snapshot.json"
owner_seckey = "$OWNER"
funding_seckey = "$FUNDING"
EOF
done
cat >> "$WORK/keeper.toml" <<EOF
poke_lag = 2
refresh_lag = 6
poll_ms = 1000
EOF

# A deliberate deviation from the SETUP.md topology: on one host every role shares machine
# A's node (and its funded deploy wallet), so `fund` works directly. On real followers the
# node wallets are empty and the L-BTC bootstrap goes through the operator - see SETUP.md.
log "funding the wallet and the keeper"
"$BIN/styx-wallet" --config "$WORK/wallet.toml" fund --sats 200000000
"$BIN/styx-wallet" --config "$WORK/keeper.toml" fund --sats 5000000
wait_status "$WORK/wallet.toml" "L-BTC: 200000000"

log "opening the vault: 4M OBOL against 1 BTC at \$120k"
"$BIN/styx-wallet" --config "$WORK/wallet.toml" open --principal 4000000 --collateral 100000000
wait_status "$WORK/wallet.toml" "vaults: 1"
wait_status "$WORK/wallet.toml" "OBOL: 4000000"

log "handing the principal to the keeper"
KEEPER_ADDR=$("$BIN/styx-wallet" --config "$WORK/keeper.toml" address | sed -n 's/^funding address: //p')
"$BIN/styx-wallet" --config "$WORK/wallet.toml" send --to "$KEEPER_ADDR" --amount 4000000
wait_status "$WORK/keeper.toml" "OBOL: 4000000"

# --- 7. the keeper daemon ---------------------------------------------------------------
"$BIN/styx-keeper" --config "$WORK/keeper.toml" > "$WORK/keeper.log" 2>&1 &
PIDS+=($!)
wait_log "Poked" "$WORK/keeper.log" 60
log "keeper on duty (poked the anchor)"

# --- 8. the crash cascade ----------------------------------------------------------------
log "crash to \$50k: the partial band"
"$ROOT/deploy/scenario-crash.sh" "$WORK/styxnet.toml" 50000
wait_log "Partial" "$WORK/keeper.log" 120
log "partial liquidation done"

log "crash to \$35k: under water"
"$ROOT/deploy/scenario-crash.sh" "$WORK/styxnet.toml" 35000
wait_log "BadDebt" "$WORK/keeper.log" 120
log "bad-debt closure done"

# --- 9. the postcondition ------------------------------------------------------------------
wait_status "$WORK/wallet.toml" "vaults: 0"
wait_status "$WORK/wallet.toml" "pot 100000000 OBOL"
# The keeper burned all its OBOL into the pot and was made whole in sats: collateral plus
# the reserve's shortfall cover, on top of the 5M it was funded with (minus fees).
wait_status "$WORK/keeper.toml" "OBOL: 0 units"
KEEPER_SATS=$("$BIN/styx-wallet" --config "$WORK/keeper.toml" status | sed -n 's/^L-BTC: \([0-9]*\) sats.*/\1/p')
if [ "${KEEPER_SATS:-0}" -le 5000000 ]; then
  echo "keeper was not compensated: $KEEPER_SATS sats" >&2
  exit 1
fi
log "cascade complete: no vaults, the pot at full supply, the keeper compensated ($KEEPER_SATS sats)"
echo "SWARM OK ($WORK)"
