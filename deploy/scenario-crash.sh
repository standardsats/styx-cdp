#!/usr/bin/env bash
# Scenario control: move every oracle's price at once through the admin endpoints listed
# in styxnet.toml. A crash deep enough walks the keeper down the ladder: refreshes stop
# passing the health gate, then partial liquidation, then bad debt.
#
# Usage: scenario-crash.sh <styxnet.toml> <usd-price>
set -euo pipefail
CONFIG=${1:?usage: scenario-crash.sh <styxnet.toml> <usd-price>}
PRICE=${2:?usage: scenario-crash.sh <styxnet.toml> <usd-price>}

URLS=$(sed -n 's/^admin_url = "\(.*\)"/\1/p' "$CONFIG")
if [ -z "$URLS" ]; then
  echo "no admin_url entries in $CONFIG" >&2
  exit 1
fi
for url in $URLS; do
  echo -n "  $url/price <- $PRICE: "
  curl -sS -X POST -H 'content-type: application/json' -d "{\"usd\":$PRICE}" "$url/price"
  echo ""
done
