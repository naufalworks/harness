#!/usr/bin/env bash
# Strict, non-deploying release verification. Every declared suite is required.
set -euo pipefail
cd "$(dirname "$0")/.."

run_suite() {
  local name=$1
  shift
  echo "[RUN] $name"
  if "$@"; then
    echo "[PASS] $name"
  else
    local status=$?
    echo "[FAIL] $name (exit $status)" >&2
    exit "$status"
  fi
}

command -v cargo >/dev/null || { echo '[BLOCKED] release: cargo is required' >&2; exit 2; }
command -v python3 >/dev/null || { echo '[BLOCKED] release: python3 is required' >&2; exit 2; }
command -v node >/dev/null || { echo '[BLOCKED] release: node is required' >&2; exit 2; }
[ -d node_modules ] || { echo '[BLOCKED] release: browser dependencies are required; run scripts/setup_browser_tests.sh' >&2; exit 2; }

run_suite local-contract env HARNESS_REQUIRE_BROWSER=1 scripts/verify_local.sh
run_suite real-browser-to-server scripts/verify_e2e.sh

echo '[PASS] strict release gate: native, contracts, HTTP, mocked-browser, and real browser-to-server suites all passed.'
echo '[INFO] no service was restarted; production promotion is a separate scripts/deploy.sh action.'
