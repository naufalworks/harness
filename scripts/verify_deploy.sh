#!/usr/bin/env bash
# Disposable deployment verification.
#
# Runs the deploy-safety suite in throwaway fixture repositories whose PATH is
# prepended with stubbed systemctl/curl/cargo/git/sha256sum, so this script never
# restarts a real unit, curls a live service, or opens a production database.
# Keep production promotion (bash scripts/deploy.sh --approve) a separate,
# explicitly approved action.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

command -v python3 >/dev/null || { echo 'BLOCKED: python3 is required.' >&2; exit 2; }
python3 tests/test_deploy_safety.py
echo 'deploy safety verification passed: fixtures/stubs only; no systemctl, curl, or production database was touched.'
