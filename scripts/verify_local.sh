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

run_suite rust-tests cargo test --locked
# P12-T04: --all-features widens coverage and -D warnings makes the zero-warning state
# self-defending. Without -D this suite passed with 42 accumulated warnings, and without
# --all-targets it cannot see unused imports consumed only by #[cfg(test)] code.
run_suite rust-clippy cargo clippy --locked --all-targets --all-features -- -D warnings
run_suite rust-build cargo build --locked
run_suite python-contracts python3 -m unittest discover -s tests -p 'test_*.py' -v
run_suite integration-smoke python3 tests/integration_smoke.py
run_suite recording-integration python3 tests/recording_integration.py
run_suite javascript-syntax node --check static/app.js

if [ -d node_modules ]; then
  run_suite mocked-browser scripts/verify_browser.sh
elif [ "${HARNESS_REQUIRE_BROWSER:-0}" = 1 ]; then
  echo '[BLOCKED] mocked-browser: run scripts/setup_browser_tests.sh first' >&2
  exit 2
else
  echo '[SKIPPED] mocked-browser: node_modules is absent; run scripts/setup_browser_tests.sh'
fi

echo '[PASS] local developer verification completed; inspect any [SKIPPED] suites above.'
