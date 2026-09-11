#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
command -v cargo >/dev/null || { echo 'BLOCKED: Rust/cargo required; Python contracts alone do not validate this release.' >&2; exit 2; }
cargo test --locked
cargo clippy --locked --all-targets
cargo build --locked
python3 -m unittest discover -s tests -p 'test_*.py' -v
python3 tests/integration_smoke.py
python3 tests/recording_integration.py
node --check static/app.js
if [ -d node_modules ]; then
  scripts/verify_browser.sh
else
  echo 'Browser suites skipped: run scripts/setup_browser_tests.sh to enable them.'
fi
echo 'Native, SQL, local mock-provider HTTP and browser gates passed.'
