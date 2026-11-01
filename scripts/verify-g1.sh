#!/usr/bin/env bash
set -euo pipefail

# Deterministic G1 evidence run.  The macOS feth/BPF smoke test is only
# executed when the privileged helper is already running; otherwise the report
# records it as skipped.  This never escalates privileges itself.

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
mkdir -p target/g1

echo "== rustfmt =="
cargo fmt --all -- --check

echo "== clippy =="
cargo clippy --workspace --all-targets -- -D warnings

echo "== tests =="
cargo test --workspace --all-targets 2>&1 | tee target/g1/tests.log

echo "== arm64 release build =="
cargo build --release --target aarch64-apple-darwin --workspace 2>&1 | tee target/g1/build.log

TEST_STATS="$(python3 - target/g1/tests.log <<'PY'
import re, sys
text = open(sys.argv[1], encoding='utf-8', errors='replace').read()
passed = failed = 0
for match in re.finditer(r'test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed', text):
    passed += int(match.group(1))
    failed += int(match.group(2))
print(f"{passed} {failed}")
PY
)"
TEST_PASSED="${TEST_STATS%% *}"
TEST_FAILED="${TEST_STATS##* }"
TEST_COUNT=$((TEST_PASSED + TEST_FAILED))

FUZZ_STATUS="skipped"
FUZZ_DETAIL="cargo-fuzz or nightly toolchain unavailable"
if command -v cargo-fuzz >/dev/null 2>&1 && rustup run nightly rustc --version >/dev/null 2>&1; then
  echo "== fuzz build =="
  (cd fuzz && cargo +nightly fuzz build) 2>&1 | tee "$ROOT/target/g1/fuzz-build.log"
  FUZZ_STATUS="passed"
  FUZZ_DETAIL=""
  for target in descriptors rndis ethernet; do
    echo "== fuzz $target =="
    if ! (cd fuzz && cargo +nightly fuzz run "$target" -- -max_total_time=8 -print_final_stats=1) \
        >"$ROOT/target/g1/fuzz-$target.log" 2>&1; then
      FUZZ_STATUS="failed"
      FUZZ_DETAIL="target $target failed"
    fi
  done
fi

echo "== net-smoke =="
set +e
NET_SMOKE_JSON="$(cargo run -q -p tether-cli -- net-smoke --json 2>/dev/null)"
NET_SMOKE_STATUS=$?
set -e
printf '%s\n' "$NET_SMOKE_JSON" > target/g1/net-smoke.json
if [ "$NET_SMOKE_STATUS" -ne 0 ]; then
  echo "net-smoke exited with $NET_SMOKE_STATUS (helper not running or smoke failed)"
fi

COMMIT="unversioned"
if git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  COMMIT="$(git -C "$ROOT" rev-parse HEAD)"
fi

python3 - "$TEST_COUNT" "$TEST_PASSED" "$TEST_FAILED" "$FUZZ_STATUS" "$FUZZ_DETAIL" "$COMMIT" <<'PY'
import json, pathlib, sys
count, passed, failed, fuzz_status, fuzz_detail, commit = sys.argv[1:]
report = {
    "g1": {
        "protocol_core": "verified" if int(failed) == 0 else "failed",
        "macos_runtime_smoke": "see target/g1/net-smoke.json",
    },
    "commit": commit,
    "tests": {"count": int(count), "passed": int(passed), "failed": int(failed)},
    "fuzz": {"status": fuzz_status, "detail": fuzz_detail,
             "targets": ["descriptors", "rndis", "ethernet"],
             "max_total_time_seconds": 8},
    "build": {"target": "aarch64-apple-darwin", "status": "passed"},
    "limitations": [
        "no real Android device",
        "macOS feth/BPF smoke requires the privileged helper",
        "no NCM/ECM data path",
        "no coverage report",
    ],
}
pathlib.Path("target/g1-report.json").write_text(json.dumps(report, indent=2) + "\n")
print(json.dumps(report, indent=2))
PY
