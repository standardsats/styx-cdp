#!/usr/bin/env bash
# Join the public Liquid-testnet deployment as a user: the node dir with generated RPC
# credentials, the wallet keys, the published deployment config, and one app.toml that
# both styx-app and styx-wallet read - everything TESTNET.md walks through by hand, in
# one idempotent pass. Existing files are kept, never overwritten, so re-running after
# a partial first run (or to see the next-steps recap) is safe.
#
# The script starts nothing: it ends by printing the start / sync / faucet / run steps
# in order.
#
# Run inside `nix develop` (elementsd, openssl, curl on PATH):
#   deploy/join-testnet.sh [dir]      # default ~/.styx
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIR=${1:-"$HOME/.styx"}
EXPLORER=https://explorer.testnet.styx.network

for tool in elementsd elements-cli curl openssl cargo; do
  command -v "$tool" >/dev/null || { echo "$tool not on PATH (run inside nix develop)"; exit 1; }
done

log() { echo "--- $*"; }

BIN="$ROOT/target/release"
if [ ! -x "$BIN/styx-wallet" ] || [ ! -x "$BIN/styx-app" ]; then
  log "building the release binaries (a few minutes on the first run)"
  (cd "$ROOT" && cargo build --release --quiet)
fi

umask 077
mkdir -p "$DIR/node"

# --- node config + RPC credentials ------------------------------------------------------
# The password is kept in $DIR/rpc-password for the app.toml step below; the node side
# stores only the salted hash (the rpcauth line).
if [ -e "$DIR/node/elements.conf" ]; then
  log "keeping the existing node config: $DIR/node/elements.conf"
else
  PASS=$(head -c 18 /dev/urandom | base64 | tr -d '/+=')
  SALT=$(head -c 16 /dev/urandom | od -An -tx1 | tr -d ' \n')
  HMAC=$(printf '%s' "$PASS" | openssl dgst -sha256 -hmac "$SALT" | awk '{print $NF}')
  sed "s|^rpcauth=.*|rpcauth=styx:$SALT\$$HMAC|" "$ROOT/deploy/liquidtestnet.elements.conf" \
    > "$DIR/node/elements.conf"
  printf '%s\n' "$PASS" > "$DIR/rpc-password"
  log "node config written: $DIR/node/elements.conf"
fi

# --- wallet keys -------------------------------------------------------------------------
if [ -e "$DIR/app.keys" ]; then
  log "keeping the existing keys: $DIR/app.keys"
else
  "$BIN/styx-wallet" keygen > "$DIR/app.keys"
  log "keys generated: $DIR/app.keys (owner + funding; back this file up)"
fi

# --- the published deployment config -----------------------------------------------------
# Two sources carry the same bytes: the download URL the docs point at, and this checkout
# (deploy/liquid-testnet.toml, under a signed commit). Prefer the download, fall back to the
# checkout, and say so when the two disagree - that means one of them is stale.
REPO_CONFIG="$ROOT/deploy/liquid-testnet.toml"
if [ -e "$DIR/liquid-testnet.toml" ]; then
  log "keeping the existing deployment config: $DIR/liquid-testnet.toml"
elif curl -fsS "$EXPLORER/config" -o "$DIR/liquid-testnet.toml.part"; then
  grep -q 'chain = "liquidtestnet"' "$DIR/liquid-testnet.toml.part" \
    || { echo "$EXPLORER/config did not return a liquidtestnet config"; exit 1; }
  mv "$DIR/liquid-testnet.toml.part" "$DIR/liquid-testnet.toml"
  log "deployment config downloaded: $DIR/liquid-testnet.toml"
  if ! diff -q "$REPO_CONFIG" "$DIR/liquid-testnet.toml" >/dev/null 2>&1; then
    log "WARNING: it does not match $REPO_CONFIG"
    log "         compare the two before you fund anything:"
    log "         diff $REPO_CONFIG $DIR/liquid-testnet.toml"
  fi
else
  cp "$REPO_CONFIG" "$DIR/liquid-testnet.toml"
  log "$EXPLORER/config unreachable - took the copy from this checkout: $REPO_CONFIG"
fi

# --- app.toml (styx-app and styx-wallet read the same file) ------------------------------
if [ -e "$DIR/app.toml" ]; then
  log "keeping the existing app config: $DIR/app.toml"
else
  [ -e "$DIR/rpc-password" ] || {
    echo "$DIR/node/elements.conf exists but $DIR/rpc-password does not:"
    echo "write app.toml yourself (see deploy/app.toml.example) or move the node dir away and re-run"
    exit 1
  }
  OWNER=$(sed -n 's/^owner_seckey = "\(.*\)"/\1/p' "$DIR/app.keys")
  FUNDING=$(sed -n 's/^funding_seckey = "\(.*\)"/\1/p' "$DIR/app.keys")
  cat > "$DIR/app.toml" <<EOF
# Written by deploy/join-testnet.sh - one config for styx-app and the styx-wallet CLI.
# Optional knobs (listen, poll_ms, [keeper], [proxy]): see deploy/app.toml.example.
styxnet = "$DIR/liquid-testnet.toml"
rpc_url = "http://127.0.0.1:18884"
rpc_user = "styx"
rpc_password = "$(cat "$DIR/rpc-password")"
snapshot = "$DIR/app-snapshot.json"
owner_seckey = "$OWNER"
funding_seckey = "$FUNDING"
EOF
  log "app config written: $DIR/app.toml"
fi

# The wallet indexes from the deployment anchor; the node must sync past it first.
ANCHOR=$(sed -n 's/^issuer_anchor_genesis = \([0-9]*\)/\1/p' "$DIR/liquid-testnet.toml")

log "done - next steps, in order:"
cat <<EOF

1. start the node (first sync takes a few hours):
     elementsd -datadir=$DIR/node

2. watch it pass the deployment height ($ANCHOR):
     elements-cli -datadir=$DIR/node getblockcount

3. print the funding address and give it to a Liquid-testnet faucet
   (for example liquidtestnet.com; the faucet pays the address DIRECTLY):
     $BIN/styx-wallet --config $DIR/app.toml address

4. run the app and open the printed page:
     $BIN/styx-app --config $DIR/app.toml

The CLI reads the same config: $BIN/styx-wallet --config $DIR/app.toml status
EOF
