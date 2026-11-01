#!/bin/sh
# Run a documented, low-volume download/latency check through the live tether.
# Usage: scripts/g2-stability-test.sh [seconds] [output]
set -u

DURATION=${1:-900}
OUT=${2:-target/g1/evidence/stability.log}
SOURCE_IP=${SOURCE_IP:-<tether-ip>}
GATEWAY=${GATEWAY:-<tether-gateway>}
mkdir -p "$(dirname "$OUT")"
: > "$OUT"

start=$(date +%s)
end=$((start + DURATION))
pings=0
ping_fail=0
curls=0
curl_fail=0

echo "# started $(date -u +%FT%TZ), duration=${DURATION}s, source=${SOURCE_IP}" >> "$OUT"
while [ "$(date +%s)" -lt "$end" ]; do
    if ping -c 1 -W 2000 "$GATEWAY" >/dev/null 2>&1; then
        pings=$((pings + 1))
    else
        ping_fail=$((ping_fail + 1))
        echo "$(date -u +%FT%TZ) ping FAIL" >> "$OUT"
    fi
    now=$(date +%s)
    if [ $((now % 60)) -lt 15 ]; then
        result=$(curl --interface "$SOURCE_IP" -sS -m 20 -o /dev/null \
            -w '%{http_code} %{remote_ip} %{time_total}' https://www.google.com 2>&1)
        rc=$?
        curls=$((curls + 1))
        if [ "$rc" -ne 0 ] || ! printf '%s' "$result" | grep -q '^200 '; then
            curl_fail=$((curl_fail + 1))
            echo "$(date -u +%FT%TZ) https FAIL rc=$rc $result" >> "$OUT"
        else
            echo "$(date -u +%FT%TZ) https OK $result" >> "$OUT"
        fi
    fi
    sleep 15
done

echo "# finished $(date -u +%FT%TZ) pings=$pings ping_fail=$ping_fail https=$curls https_fail=$curl_fail" >> "$OUT"
[ "$ping_fail" -eq 0 ] && [ "$curl_fail" -eq 0 ]
