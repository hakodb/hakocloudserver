#!/usr/bin/env bash
# netlog [iface] [seconds] — RX/TX Mbps sampler for sync-stress capacity context.
# default iface: the default-route one; seconds default 60.
set -u
iface=${1:-}
secs=${2:-60}
if [ -z "$iface" ]; then
  iface=$(ip -o route show default | head -1 | sed 's/.*dev \([^ ]*\).*/\1/')
fi
base="/sys/class/net/$iface"
[ -d "$base" ] || { echo "no iface $iface"; exit 1; }
echo "NETLOG start iface=$iface seconds=$secs"
rx_prev=$(cat "$base/statistics/rx_bytes")
tx_prev=$(cat "$base/statistics/tx_bytes")
end=$(( $(date +%s) + secs ))
while [ "$(date +%s)" -lt "$end" ]; do
  sleep 1
  rx=$(cat "$base/statistics/rx_bytes"); tx=$(cat "$base/statistics/tx_bytes")
  echo "NET ts=$(date +%s) iface=$iface rx_mbps=$(( (rx-rx_prev)*8/1000000 )) tx_mbps=$(( (tx-tx_prev)*8/1000000 ))"
  rx_prev=$rx; tx_prev=$tx
done
echo "NETLOG end iface=$iface"
