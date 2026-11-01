#!/bin/sh
# G2 criterion 3: prove that the Mac has no other Internet uplink while
# DNS/HTTPS traffic flows through the Android RNDIS tether.
#
# Run as the logged-in user.  Root commands use the locally granted
# `sudo -n security execute-with-privileges` path; the tether helper is already
# running as a LaunchDaemon.  The script restores the original Wi-Fi power
# state and exits non-zero if any assertion fails.
set -u

OUT=${1:-target/g1/evidence/no-uplink}
BIN=${BIN:-target/aarch64-apple-darwin/release/android-tether}
NETWORK_SETUP=/usr/sbin/networksetup
ROUTE=/sbin/route
FAIL=0
WIFI_CHANGED=0
WATCHDOG=0

mkdir -p "$OUT"
WORK=$(mktemp -d "${TMPDIR:-/tmp}/android-tether-g2.XXXXXX") || exit 1
chmod 700 "$WORK"
case "$WORK" in
    ""|*[!A-Za-z0-9/._-]*)
        echo "unsafe temporary directory name: $WORK" >&2
        exit 1
        ;;
esac

asroot() {
    sudo -n /usr/bin/security execute-with-privileges /bin/sh -c "$1" </dev/null
}

# Start a root watchdog without interpolating the temporary path into the
# privileged command string.  WORK is passed as an environment variable and
# the inner shell expands it; the path is additionally validated above.
start_watchdog() {
    WATCHDOG_SCRIPT='nohup /bin/sh -c '\''sleep 300; if [ -e "$WORK/cancel" ]; then rm -rf "$WORK"; else /usr/sbin/networksetup -setairportpower en0 on; rm -rf "$WORK"; fi'\'' >/dev/null 2>&1 & echo $! > "$WORK/watchdog.pid"'
    sudo -n /usr/bin/security execute-with-privileges \
        /usr/bin/env WORK="$WORK" /bin/sh -c "$WATCHDOG_SCRIPT" </dev/null
    i=0
    while [ ! -s "$WORK/watchdog.pid" ]; do
        i=$((i + 1))
        [ "$i" -ge 20 ] && return 1
        sleep 0.1
    done
    return 0
}

wifi_is_on() {
    "$NETWORK_SETUP" -getairportpower en0 2>/dev/null | grep -q 'On$'
}

wifi_wait_on() {
    i=0
    while ! ifconfig en0 2>/dev/null | grep -q 'inet '; do
        i=$((i + 1))
        [ "$i" -ge 30 ] && return 1
        sleep 1
    done
    return 0
}

wifi_wait_off() {
    i=0
    while ifconfig en0 2>/dev/null | grep -q 'inet '; do
        i=$((i + 1))
        [ "$i" -ge 20 ] && return 1
        sleep 1
    done
    return 0
}

fail() {
    echo "FAIL: $*" | tee -a "$OUT/summary.txt" >&2
    FAIL=$((FAIL + 1))
}

restore_wifi() {
    [ "$WIFI_CHANGED" -eq 1 ] || return 0
    if ! asroot "$NETWORK_SETUP -setairportpower en0 on" >/dev/null 2>&1; then
        fail "could not re-enable Wi-Fi"
        return 1
    fi
    if ! wifi_wait_on; then
        fail "Wi-Fi did not regain an IPv4 address"
        return 1
    fi
    WIFI_CHANGED=0
    if [ "$WATCHDOG" -eq 1 ]; then
        # Let the watchdog remove WORK after it observes this marker.
        : > "$WORK/cancel"
    else
        rm -rf "$WORK"
    fi
    return 0
}

cleanup() {
    rc=$?
    restore_wifi || rc=1
    [ "$WATCHDOG" -eq 1 ] || rm -rf "$WORK"
    exit "$rc"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

if ! "$BIN" status --json 2>/dev/null | grep -q '"connected": true'; then
    echo "FAIL: tether is not connected" >&2
    exit 1
fi

if wifi_is_on; then
    WIFI_CHANGED=1
    # Watchdog: if this script dies, restore Wi-Fi after 5 min.  Abort before
    # touching the radio if the watchdog could not be started.
    if ! start_watchdog; then
        fail "could not start the Wi-Fi restore watchdog"
        exit 1
    fi
    WATCHDOG=1
fi

echo "== before ==" | tee "$OUT/summary.txt"
{
    ifconfig en0 | grep -E 'status|inet ' || true
    $ROUTE -n get default 2>&1
    "$BIN" status --json
} >> "$OUT/summary.txt" 2>&1

echo "== disabling Wi-Fi (en0) ==" | tee -a "$OUT/summary.txt"
if [ "$WIFI_CHANGED" -eq 1 ]; then
    if ! asroot "$NETWORK_SETUP -setairportpower en0 off" >> "$OUT/summary.txt" 2>&1; then
        fail "could not disable Wi-Fi"
    fi
    if ! wifi_wait_off; then
        fail "en0 still has an IPv4 address after disabling Wi-Fi"
    fi
fi

echo "== no-uplink state ==" | tee -a "$OUT/summary.txt"
{
    ifconfig en0 2>&1 | grep -E 'status|inet ' || true
    scutil --nwi
    $ROUTE -n get default
    scutil --dns | sed -n '1,30p'
} >> "$OUT/summary.txt" 2>&1

if ifconfig en0 2>/dev/null | grep -q 'inet '; then
    fail "en0 still has an IPv4 address"
fi
NWI_INTERFACES=$(scutil --nwi | sed -n 's/^Network interfaces: //p')
if [ "$NWI_INTERFACES" != "feth100" ]; then
    fail "active interfaces are '$NWI_INTERFACES', expected only feth100"
fi
if ! $ROUTE -n get default 2>/dev/null | grep -q 'interface: feth100'; then
    fail "default route is not on feth100"
fi
ROUTE_8=$(route -n get 8.8.8.8 2>/dev/null | awk '/interface:/{print $2}')
if [ "$ROUTE_8" != "feth100" ]; then
    fail "route to 8.8.8.8 uses '${ROUTE_8:-unknown}', expected feth100"
fi
# A local bridge (e.g. a VM) may keep a link-scoped default route without
# being an Internet uplink.  scutil --nwi is the authoritative active list;
# require it to contain exactly feth100.
if ! scutil --nwi | grep -q 'Network interfaces: feth100$'; then
    fail "scutil --nwi does not end with 'Network interfaces: feth100'"
fi

echo "== DNS via phone ==" | tee -a "$OUT/summary.txt"
{
    dig +time=5 +tries=1 +short @<tether-gateway> example.com
    echo "dig_rc=$?"
    dscacheutil -q host -a name example.com
    echo "dscacheutil_rc=$?"
} > "$OUT/dns.txt" 2>&1
if ! grep -q '^[0-9][0-9.]*$' "$OUT/dns.txt"; then
    fail "phone DNS returned no IPv4 address"
fi
if ! grep -q 'ip_address:' "$OUT/dns.txt"; then
    fail "system resolver returned no IPv4 address"
fi

echo "== ping 8.8.8.8 ==" | tee -a "$OUT/summary.txt"
ping -c 4 -W 3000 8.8.8.8 > "$OUT/ping-8.8.8.8.txt" 2>&1
ping_rc=$?
echo "ping_rc=$ping_rc" >> "$OUT/ping-8.8.8.8.txt"
[ "$ping_rc" -eq 0 ] || fail "ping 8.8.8.8 failed"

echo "== HTTPS without interface binding ==" | tee -a "$OUT/summary.txt"
curl -sS -m 25 -o /dev/null \
    -w 'google http_code=%{http_code} remote_ip=%{remote_ip} time=%{time_total}\n' \
    https://www.google.com > "$OUT/https-google.txt" 2>&1
curl_rc=$?
echo "curl_rc=$curl_rc" >> "$OUT/https-google.txt"
[ "$curl_rc" -eq 0 ] || fail "HTTPS request failed"
grep -q 'http_code=200' "$OUT/https-google.txt" || fail "HTTPS request did not return 200"

curl -sS -m 25 https://ifconfig.me/ip > "$OUT/public-ip.txt" 2>&1
ip_rc=$?
echo "curl_rc=$ip_rc" >> "$OUT/public-ip.txt"
[ "$ip_rc" -eq 0 ] || fail "public-IP request failed"
grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+' "$OUT/public-ip.txt" || fail "public-IP response is not an IPv4 address"

echo "== restoring Wi-Fi ==" | tee -a "$OUT/summary.txt"
restore_wifi || true

echo "== after ==" | tee -a "$OUT/summary.txt"
{
    ifconfig en0 2>&1 | grep -E 'status|inet ' || true
    $ROUTE -n get default 2>&1
    "$BIN" status --json
} >> "$OUT/summary.txt" 2>&1

echo "== results ==" | tee -a "$OUT/summary.txt"
cat "$OUT/dns.txt" "$OUT/ping-8.8.8.8.txt" "$OUT/https-google.txt" "$OUT/public-ip.txt" \
    >> "$OUT/summary.txt"

if [ "$FAIL" -ne 0 ]; then
    echo "RESULT: FAIL ($FAIL assertions)" | tee -a "$OUT/summary.txt"
    exit 1
fi
echo "RESULT: PASS" | tee -a "$OUT/summary.txt"
