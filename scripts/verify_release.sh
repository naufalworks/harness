#!/usr/bin/env bash
# Release gate. Strict by default: every declared lane must actually run.
#
# Why this exists: the previous gate ran the mocked browser suites only when
# node_modules happened to exist, never ran the real browser E2E lane, and then
# printed one "all gates passed" line regardless. A green exit therefore meant
# different coverage on different hosts and overstated what ran. This script
# reports per-suite PASS/FAIL/SKIP, refuses to run in strict mode without its
# declared runtimes, and only prints a pass line when nothing was skipped.
#
# Usage:
#   bash scripts/verify_release.sh          # strict gate (requires cargo, node, browser deps)
#   bash scripts/verify_release.sh --dev    # local check; may SKIP browser/E2E, never claims a release pass
# Exit codes: 0 all required suites passed; 1 a suite failed; 2 a required runtime is missing (strict).
set -uo pipefail

root=$(cd "$(dirname "$0")/.." && pwd) || exit 2
cd "$root" || exit 2

mode=release
for arg in "$@"; do
  case $arg in
    --dev) mode=dev ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

passed=()
failed=()
skipped=()

run() {
  local name=$1
  shift
  echo "== $name: $*"
  if "$@"; then
    echo "[PASS] $name"
    passed+=("$name")
  else
    echo "[FAIL] $name"
    failed+=("$name")
  fi
}

skip() {
  echo "[SKIP] $1 ($2)"
  skipped+=("$1")
}

browser_ready() {
  command -v node >/dev/null 2>&1 || return 1
  [ -d node_modules ] || return 1
  if [ -z "${CHROMIUM_PATH:-}" ]; then
    CHROMIUM_PATH="$(node -e "process.stdout.write(require('playwright').chromium.executablePath())" 2>/dev/null || true)"
  fi
  [ -n "${CHROMIUM_PATH:-}" ] && [ -x "${CHROMIUM_PATH:-}" ]
}

command -v cargo >/dev/null || { echo 'BLOCKED: Rust/cargo required; Python contracts alone do not validate this release.' >&2; exit 2; }
command -v python3 >/dev/null || { echo 'BLOCKED: python3 is required.' >&2; exit 2; }

run native-tests cargo test --locked
run native-lints cargo clippy --locked --all-targets
run native-build cargo build --locked
run python-contracts python3 -m unittest discover -s tests -p 'test_*.py' -v
run integration-smoke python3 tests/integration_smoke.py
run recording-integration python3 tests/recording_integration.py
run js-syntax node --check static/app.js

if browser_ready; then
  export CHROMIUM_PATH
  run browser-mocked scripts/verify_browser.sh
  run browser-e2e scripts/verify_e2e.sh
else
  reason='node, node_modules or Playwright Chromium is unavailable'
  if [ "$mode" = release ]; then
    echo "BLOCKED: strict release gate requires the browser runtime: $reason" >&2
    echo "Install it with scripts/setup_browser_tests.sh, or run --dev for a local check that skips the browser and E2E lanes." >&2
    exit 2
  fi
  skip browser-mocked "$reason"
  skip browser-e2e "$reason"
fi

echo "-----"
echo "passed:  ${passed[*]:-none}"
echo "failed:  ${failed[*]:-none}"
echo "skipped: ${skipped[*]:-none}"

if [ "${#failed[@]}" -gt 0 ]; then
  echo "verify_release: FAILED — suites failed: ${failed[*]}" >&2
  exit 1
fi
if [ "${#skipped[@]}" -gt 0 ]; then
  echo "verify_release: DEV CHECK ONLY — ${#skipped[@]} suite(s) skipped (${skipped[*]}). This is NOT a release gate pass; run without --dev on a host with the browser runtime."
  exit 0
fi
echo "verify_release: release gate PASSED — ${#passed[@]} suites ran (native tests/lints/build, python contracts, integration smoke, recording integration, JS syntax, mocked browser, real browser E2E), 0 skipped."
