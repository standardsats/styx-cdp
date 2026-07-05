#!/usr/bin/env bash
# styxnet block production: one block per interval on the producer machine. Custom Elements
# chains sign blocks with the default OP_TRUE signblockscript, so generatetoaddress works
# and any connected node could technically produce - running this on one machine is an
# operational convention, not consensus.
#
# Usage: produce-blocks.sh <datadir> [interval-seconds]
set -euo pipefail
DATADIR=${1:?usage: produce-blocks.sh <datadir> [interval-seconds]}
INTERVAL=${2:-10}
CLI=${ELEMENTS_CLI:-elements-cli}
ADDR=$($CLI -datadir="$DATADIR" getnewaddress)
echo "producing to $ADDR every ${INTERVAL}s"
while true; do
  $CLI -datadir="$DATADIR" generatetoaddress 1 "$ADDR" >/dev/null
  sleep "$INTERVAL"
done
