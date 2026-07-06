#!/usr/bin/env bash
# styxnet block production: one block per interval on the producer machine. Custom Elements
# chains sign blocks with the default OP_TRUE signblockscript, so generatetoaddress works
# and any connected node could technically produce - running this on one machine is an
# operational convention, not consensus.
#
# Mines to its own wallet (created if absent): with several wallets loaded on the node a
# bare getnewaddress is ambiguous, and the producer must not share the deploy wallet.
#
# Usage: produce-blocks.sh <datadir> [interval-seconds] [wallet]
set -euo pipefail
DATADIR=${1:?usage: produce-blocks.sh <datadir> [interval-seconds] [wallet]}
INTERVAL=${2:-10}
WALLET=${3:-producer}
CLI=${ELEMENTS_CLI:-elements-cli}
$CLI -datadir="$DATADIR" createwallet "$WALLET" >/dev/null 2>&1 \
  || $CLI -datadir="$DATADIR" loadwallet "$WALLET" >/dev/null 2>&1 \
  || true
ADDR=$($CLI -datadir="$DATADIR" -rpcwallet="$WALLET" getnewaddress)
echo "producing to $ADDR every ${INTERVAL}s"
while true; do
  $CLI -datadir="$DATADIR" generatetoaddress 1 "$ADDR" >/dev/null
  sleep "$INTERVAL"
done
