#!/usr/bin/env bash
# One health pass over a deployment: every oracle's admin surface (reachable, publishing,
# feed not stale, height advancing in lockstep) and the relay. Exits non-zero listing the
# alerts - cron it or run it from the styx-monitor systemd timer and let the journal (or
# an OnFailure= hook) carry the alarm.
#
# Usage: check-health.sh <styxnet.toml> [max-feed-age-secs]
set -euo pipefail
CONFIG=${1:?usage: check-health.sh <styxnet.toml> [max-feed-age-secs]}
MAX_FEED_AGE=${2:-120}

ALERTS=()
alert() { ALERTS+=("$1"); }

field() { # field <json> <name> -> numeric value or empty
  sed -n "s/.*\"$2\":\([0-9][0-9]*\).*/\1/p" <<< "$1"
}

HEIGHTS=()
URLS=$(sed -n 's/^admin_url = "\(.*\)"/\1/p' "$CONFIG")
[ -n "$URLS" ] || { echo "no admin_url entries in $CONFIG" >&2; exit 1; }
for url in $URLS; do
  if ! H=$(curl -sS -m 5 "$url/health" 2>&1); then
    alert "oracle $url unreachable: $H"
    continue
  fi
  grep -q '"publishing":true' <<< "$H" || alert "oracle $url not publishing: $H"
  if grep -q '"source":"feed"' <<< "$H"; then
    AGE=$(field "$H" feed_age_secs)
    if [ -n "$AGE" ] && [ "$AGE" -gt "$MAX_FEED_AGE" ]; then
      alert "oracle $url feed silent for ${AGE}s"
    fi
  fi
  HEIGHTS+=("$(field "$H" height)")
done

# The published heights must move in lockstep: a straggler is a stuck node or daemon.
if [ "${#HEIGHTS[@]}" -gt 1 ]; then
  MIN=${HEIGHTS[0]} MAX=${HEIGHTS[0]}
  for h in "${HEIGHTS[@]}"; do
    [ "$h" -lt "$MIN" ] && MIN=$h
    [ "$h" -gt "$MAX" ] && MAX=$h
  done
  if [ $((MAX - MIN)) -gt 2 ]; then
    alert "oracle heights diverge: min $MIN max $MAX"
  fi
fi

# The relay answers NIP-11 over plain HTTP on its websocket endpoint.
RELAYS=$(sed -n 's/.*relays = \[\"\(ws[s]*:[^"]*\)\".*/\1/p' "$CONFIG")
for relay in $RELAYS; do
  http_url=${relay/wss:/https:}
  http_url=${http_url/ws:/http:}
  if ! curl -sS -m 5 -H 'Accept: application/nostr+json' -o /dev/null "$http_url"; then
    alert "relay $relay unreachable"
  fi
done

if [ "${#ALERTS[@]}" -gt 0 ]; then
  printf 'ALERT: %s\n' "${ALERTS[@]}" >&2
  exit 1
fi
echo "health OK: ${#HEIGHTS[@]} oracles publishing at height ~${HEIGHTS[0]:-?}"
