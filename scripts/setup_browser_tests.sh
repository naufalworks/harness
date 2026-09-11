#!/usr/bin/env bash
# Installs the browser-test runtime: Playwright, its Chromium build, and the system
# libraries Chromium needs. Run once per machine; safe to re-run.
set -euo pipefail
cd "$(dirname "$0")/.."

command -v node >/dev/null || { echo 'BLOCKED: node is required for the browser suites.' >&2; exit 2; }
if ! command -v npm >/dev/null; then
  echo 'BLOCKED: npm is required. On Debian/Ubuntu: apt-get install -y --no-install-recommends npm' >&2
  exit 2
fi

npm install --no-audit --no-fund
npx playwright install chromium
# System libraries; needs root and apt. Non-fatal so a pre-provisioned host still works.
npx playwright install-deps chromium || echo 'note: install-deps did not complete; verify Chromium can launch.'

echo 'Browser runtime ready. Verify with: scripts/verify_browser.sh'
