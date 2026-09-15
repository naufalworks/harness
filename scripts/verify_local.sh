#!/usr/bin/env bash
# Developer verification. Missing browser dependencies are reported as skipped,
# never as passed. Use verify_release.sh for the fail-closed release contract.
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

command -v cargo >/dev/null || { echo '[BLOCKED] rust: cargo is required' >&2; exit 2; }
command -v python3 >/dev/null || { echo '[BLOCKED] python: python3 is required' >&2; exit 2; }
command -v node >/dev/null || { echo '[BLOCKED] javascript: node is required' >&2; exit 2; }

# P12-T05b: formatting first. It is the cheapest failure in the suite and it
# fails for a reason that never requires reading a test log.
run_suite rust-fmt cargo fmt --all -- --check
run_suite rust-tests cargo test --locked
# P12-T04: --all-features widens coverage and -D warnings makes the zero-warning state
# self-defending. Without -D this suite passed with 42 accumulated warnings, and without
# --all-targets it cannot see unused imports consumed only by #[cfg(test)] code.
run_suite rust-clippy cargo clippy --locked --all-targets --all-features -- -D warnings
run_suite rust-build cargo build --locked
# P12-T05b: ResourceWarning is promoted to an error so a leaked socket, file or
# database handle fails here rather than turning into flake somewhere later.
# Exit status alone is NOT sufficient evidence: a warning raised inside a
# deallocator is reported as "Exception ignored" and the process still exits 0.
# This wrapper therefore also fails when the text appears in the output, which is
# how nine leaked sqlite connections were found in recording_integration.py.
run_python_suite() {
  local name=$1
  shift
  echo "[RUN] $name"
  local log
  log=$(mktemp)
  if ! python3 -W error::ResourceWarning "$@" >"$log" 2>&1; then
    cat "$log"
    echo "[FAIL] $name (non-zero exit)" >&2
    rm -f "$log"
    exit 1
  fi
  if grep -q 'ResourceWarning' "$log"; then
    cat "$log"
    grep -n 'ResourceWarning' "$log" >&2
    echo "[FAIL] $name: leaked resources reported from a deallocator, where -W error cannot fail the process" >&2
    rm -f "$log"
    exit 1
  fi
  cat "$log"
  rm -f "$log"
  echo "[PASS] $name"
}

run_python_suite python-contracts -m unittest discover -s tests -p 'test_*.py' -v
run_python_suite integration-smoke tests/integration_smoke.py
run_python_suite recording-integration tests/recording_integration.py
run_suite shell-syntax bash -n scripts/deploy.sh scripts/release.sh scripts/setup_browser_tests.sh scripts/verify_browser.sh scripts/verify_e2e.sh scripts/verify_local.sh scripts/verify_release.sh
run_suite supply-chain python3 scripts/check_supply_chain.py
run_suite release-quality python3 scripts/check_release_quality.py
run_suite javascript-syntax node --check static/api.js static/app.js

if [ -d node_modules ]; then
  run_suite mocked-browser scripts/verify_browser.sh
elif [ "${HARNESS_REQUIRE_BROWSER:-0}" = 1 ]; then
  echo '[BLOCKED] mocked-browser: run scripts/setup_browser_tests.sh first' >&2
  exit 2
else
  echo '[SKIPPED] mocked-browser: node_modules is absent; run scripts/setup_browser_tests.sh'
fi

echo '[PASS] local developer verification completed; inspect any [SKIPPED] suites above.'
