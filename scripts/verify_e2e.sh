#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo >/dev/null || { echo 'BLOCKED: cargo is required.' >&2; exit 2; }
command -v node >/dev/null || { echo 'BLOCKED: node is required.' >&2; exit 2; }
[ -d node_modules ] || { echo 'BLOCKED: run scripts/setup_browser_tests.sh first.' >&2; exit 2; }
cargo build --locked
if [ -z "${CHROMIUM_PATH:-}" ]; then
  CHROMIUM_PATH="$(node -e "process.stdout.write(require('playwright').chromium.executablePath())")"
fi
export CHROMIUM_PATH
[ -x "$CHROMIUM_PATH" ] || { echo "BLOCKED: Chromium not found at $CHROMIUM_PATH" >&2; exit 2; }
node tests/browser_e2e.cjs
node tests/browser_e2e_failure.cjs
