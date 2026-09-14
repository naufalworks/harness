#!/usr/bin/env bash
# Build, promote, and verify one clean release. If candidate verification fails,
# restore the previous executable only when the database schema remains compatible.
set -euo pipefail
cd "$(dirname "$0")/.."

unit=${HARNESS_UNIT:-harness}
bin=$PWD/target/release/harness
deploy_dir=$PWD/.harness/deploy
previous_bin=$deploy_dir/previous-harness
previous_manifest=$deploy_dir/previous.json

for command in cargo curl git jq python3 sha256sum systemctl; do
  command -v "$command" >/dev/null || { echo "BLOCKED: $command is required" >&2; exit 2; }
done

dirty=$(git status --porcelain 2>/dev/null | wc -l)
if [ "$dirty" -gt 0 ] && [ "${HARNESS_ALLOW_DIRTY_DEPLOY:-0}" != 1 ]; then
  echo "BLOCKED: working tree has $dirty uncommitted change(s); commit them or explicitly set HARNESS_ALLOW_DIRTY_DEPLOY=1" >&2
  exit 2
fi
if [ "$dirty" -gt 0 ]; then
  echo "WARNING: explicitly deploying $dirty uncommitted change(s); commit identity does not attest the full source state" >&2
fi

set -a
# .env is deployment-local and deliberately not in the repository, so the
# linter cannot follow it. The variables it must define are validated below
# with ${VAR:?...} expansions.
# shellcheck source=/dev/null
. ./.env
set +a
base="http://${HARNESS_ADDR:-127.0.0.1:8080}"
auth="Authorization: Bearer ${HARNESS_AUTH_TOKEN:?HARNESS_AUTH_TOKEN missing from .env}"
database=${HARNESS_DB:-data/harness_v2.db}

exec_start=$(systemctl show -p ExecStart --value "$unit")
case $exec_start in
*"$bin"*) ;;
*) echo "BLOCKED: $unit does not start $bin (ExecStart: $exec_start)" >&2; exit 2 ;;
esac
[ -x "$bin" ] || { echo "BLOCKED: current executable is unavailable at $bin" >&2; exit 2; }

previous_health=$(curl -fsS -H "$auth" "$base/health") || {
  echo 'BLOCKED: current release health is unavailable; rollback cannot be attested' >&2
  exit 2
}
previous_ready=$(jq -r '.ready' <<<"$previous_health")
previous_commit=$(jq -r '.commit' <<<"$previous_health")
previous_hash=$(jq -r '.binary_sha256' <<<"$previous_health")
previous_schema=$(jq -r '.schema_version' <<<"$previous_health")
[ "$previous_ready" = true ] || { echo "BLOCKED: current release is unready: $previous_health" >&2; exit 2; }
[[ "$previous_schema" =~ ^[0-9]+$ ]] || { echo "BLOCKED: current schema is not numeric: $previous_health" >&2; exit 2; }
[ "$(sha256sum "$bin" | cut -d' ' -f1)" = "$previous_hash" ] || {
  echo 'BLOCKED: current on-disk executable differs from served health identity' >&2
  exit 2
}

install -d -m 700 "$deploy_dir"
install -m 700 "$bin" "$previous_bin"
jq -n \
  --arg captured_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg commit "$previous_commit" \
  --arg hash "$previous_hash" \
  --argjson schema "$previous_schema" \
  '{captured_at:$captured_at,commit:$commit,binary_sha256:$hash,schema_version:$schema}' \
  > "$previous_manifest.tmp"
chmod 600 "$previous_manifest.tmp"
mv "$previous_manifest.tmp" "$previous_manifest"

echo "building candidate release from $(git rev-parse --short HEAD)"
cargo build --locked --release
built=$(sha256sum "$bin" | cut -d' ' -f1)
expected_commit=$(git rev-parse HEAD)
latest_schema=$(python3 - <<'PY'
from pathlib import Path
print(max(int(path.name.split('_', 1)[0]) for path in Path('migrations').glob('*.sql')))
PY
)

wait_for_identity() {
  local expected_commit=$1 expected_hash=$2 expected_schema=$3
  health=''
  for _ in $(seq 1 20); do
    if health=$(curl -fsS -H "$auth" "$base/health" 2>/dev/null) \
      && [ "$(jq -r '.ready' <<<"$health")" = true ] \
      && [ "$(jq -r '.commit' <<<"$health")" = "$expected_commit" ] \
      && [ "$(jq -r '.binary_sha256' <<<"$health")" = "$expected_hash" ] \
      && [ "$(jq -r '.schema_version' <<<"$health")" = "$expected_schema" ] \
      && [ "$(jq -r '.database.ready and .workers.recording and .workers.extraction' <<<"$health")" = true ]; then
      return 0
    fi
    sleep 1
  done
  return 1
}

current_schema() {
  python3 - "$database" <<'PY'
import sqlite3, sys
try:
    with sqlite3.connect(sys.argv[1]) as connection:
        print(connection.execute('PRAGMA user_version').fetchone()[0])
except Exception:
    print(-1)
PY
}

rollback_or_stop() {
  local reason=$1 schema action pid running
  schema=$(current_schema)
  action=$(python3 scripts/rollback_policy.py decision "$previous_schema" "$schema" | jq -r '.action')
  systemctl stop "$unit" || true
  if [ "$action" = restore_previous_binary ]; then
    python3 scripts/rollback_policy.py restore-binary "$previous_bin" "$bin"
    systemctl restart "$unit"
    if wait_for_identity "$previous_commit" "$previous_hash" "$previous_schema"; then
      pid=$(systemctl show -p MainPID --value "$unit")
      running=$(sha256sum "/proc/$pid/exe" | cut -d' ' -f1)
      [ "$running" = "$previous_hash" ] || { echo "RECOVERY FAILED: restored service hash $running differs from $previous_hash" >&2; exit 1; }
      echo "FAILED: candidate verification failed ($reason); previous release $previous_commit is serving again with schema $schema" >&2
      exit 1
    fi
    echo "RECOVERY FAILED: candidate failed ($reason) and the previous release did not become ready" >&2
    exit 1
  fi
  echo "RECOVERY REQUIRED: candidate failed ($reason) after schema advanced from $previous_schema to $schema; service is stopped and no database restore was attempted" >&2
  echo "Backup created at: ${HARNESS_BACKUP_CREATED_AT:-not-attested}; writes accepted after backup: ${HARNESS_WRITES_AFTER_BACKUP:-unknown}" >&2
  echo 'Database restore requires explicit owner approval after reviewing backup age and post-backup writes.' >&2
  exit 1
}

if ! systemctl restart "$unit"; then
  rollback_or_stop 'restart failed'
fi
if ! wait_for_identity "$expected_commit" "$built" "$latest_schema"; then
  rollback_or_stop 'readiness or build identity mismatch'
fi
pid=$(systemctl show -p MainPID --value "$unit")
running=$(sha256sum "/proc/$pid/exe" | cut -d' ' -f1)
[ "$running" = "$built" ] || rollback_or_stop 'live executable hash mismatch'
curl -fsS -o /dev/null -H "$auth" "$base/scopes" || rollback_or_stop 'authenticated API smoke failed'
refused=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "$auth" -H 'Content-Type: application/json' -d '[]' "$base/chat/submit")
[ "$refused" = 400 ] || rollback_or_stop "non-object body returned $refused"

echo "deployed $(git rev-parse --short HEAD) to $unit: pid $pid, release sha256 $built, schema $latest_schema, readiness verified, API answering, non-object body refused with 400"
