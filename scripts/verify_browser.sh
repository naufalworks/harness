#!/usr/bin/env bash
# Runs the mocked-browser fixtures against the Playwright Chromium build.
# Setup first: scripts/setup_browser_tests.sh
set -euo pipefail
cd "$(dirname "$0")/.."

[ -d node_modules ] || { echo 'BLOCKED: run scripts/setup_browser_tests.sh first.' >&2; exit 2; }

# Resolve the browser from Playwright itself so no machine-specific path is hard-coded.
if [ -z "${CHROMIUM_PATH:-}" ]; then
  CHROMIUM_PATH="$(node -e "process.stdout.write(require('playwright').chromium.executablePath())")"
fi
export CHROMIUM_PATH
[ -x "$CHROMIUM_PATH" ] || { echo "BLOCKED: Chromium not found at $CHROMIUM_PATH" >&2; exit 2; }
echo "Chromium: $CHROMIUM_PATH"

node tests/recording_ui.cjs
node tests/ui_smoke.cjs
echo 'Browser suites passed.'
