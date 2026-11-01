#!/usr/bin/env bash
set -euo pipefail

# Hardware-free macOS integration smoke test.  Requires sudo; intended for a
# developer machine or a CI runner that allows passwordless sudo.  The helper
# creates a feth pair, a BPF descriptor and a DHCP network service, then the CLI
# verifies link up/down and cleanup.  No phone is attached.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

cargo build --release --target aarch64-apple-darwin --workspace
BIN="target/aarch64-apple-darwin/release"
SOCKET="/tmp/android-tether-helper-smoke-$$.sock"

cleanup() {
  if [ -n "${HELPER_PID:-}" ]; then
    sudo kill "$HELPER_PID" 2>/dev/null || true
    wait "$HELPER_PID" 2>/dev/null || true
  fi
  sudo rm -f "$SOCKET" 2>/dev/null || true
}
trap cleanup EXIT

sudo "$BIN/android-tether-helper" serve \
  --socket "$SOCKET" \
  --allow-uid "$(id -u)" &
HELPER_PID=$!

for _ in $(seq 1 50); do
  [ -S "$SOCKET" ] && break
  sleep 0.1
done
if [ ! -S "$SOCKET" ]; then
  echo "helper socket did not appear" >&2
  exit 1
fi

"$BIN/android-tether" --helper "$SOCKET" net-smoke --json
