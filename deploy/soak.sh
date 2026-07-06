#!/usr/bin/env bash
# The soak: the swarm topology on LIVE exchange feeds. The system idles on real market
# data under the health check for the given duration, then runs the crash cascade with
# prices derived from the live price and hands the oracles back to the market.
#
# One exchange per slot mirrors the real deployment; on a host with partial egress, list
# the reachable ones (repeats are fine for a rehearsal, dishonest for production):
#   SOAK_BACKENDS="coinbase binance kraken bitstamp bitfinex" deploy/soak.sh 3600
#
# Usage: soak.sh [seconds] [workdir]
set -euo pipefail
SECS=${1:-3600}
BACKENDS=${SOAK_BACKENDS:-"coinbase binance kraken bitstamp bitfinex"}
exec env SWARM_BACKENDS="$BACKENDS" SWARM_SOAK_SECS="$SECS" \
  "$(dirname "$0")/swarm.sh" "${2:-}"
