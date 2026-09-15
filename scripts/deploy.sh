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
# P16-T03: a deployment is itself a recorded event with a causal trail. These calls record it
# against the release that is CURRENTLY serving, which is the only server that exists at this
# point; the trail therefore describes the deployment as it happens rather than being
# reconstructed afterwards from a server that may never come up. Recording is best-effort on
# purpose: a provenance write must never be the reason a deployment fails, so every call is
# guarded and a failure leaves the phase unrecorded rather than aborting the release.
deployment_id="deploy-$(date -u +%Y%m%dT%H%M%SZ)-$(git rev-parse --short HEAD)"
record_phase() {
  curl -fsS -m 5 -X POST -H "$auth" -H 'Content-Type: application/json' -d "$1" "$base/deployments" 2>/dev/null | jq -r '.event_id // empty' 2>/dev/null || true
}
build_event=$(record_phase "$(jq -nc --arg d "$deployment_id" --arg c "$(git rev-parse HEAD)" --argjson s "$(python3 - <<'PY'
from pathlib import Path
print(max(int(path.name.split('_', 1)[0]) for path in Path('migrations').glob('*.sql')))
PY
)" '{deployment_id:$d,phase:"build",status:"started",commit:$c,schema_version:$s,detail:"cargo build --locked --release"}')")
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
  [ -n "${build_event:-}" ] && record_phase "$(jq -nc --arg e "$build_event" '{event_id:$e,status:"succeeded"}')" >/dev/null
  rollback_or_stop 'restart failed'
fi
if ! wait_for_identity "$expected_commit" "$built" "$latest_schema"; then
  # The candidate never reported the identity it was built with. Recorded as an anomaly on the
  # phase itself rather than as a bare failure, so the trail says WHICH check refused it.
  [ -n "${build_event:-}" ] && record_phase "$(jq -nc --arg e "$build_event" '{event_id:$e,status:"failed",detail:"readiness or build identity mismatch",anomaly_identity_mismatch:true,anomaly_unready:true}')" >/dev/null
  rollback_or_stop 'readiness or build identity mismatch'
fi
pid=$(systemctl show -p MainPID --value "$unit")
running=$(sha256sum "/proc/$pid/exe" | cut -d' ' -f1)
if [ "$running" != "$built" ]; then
  [ -n "${build_event:-}" ] && record_phase "$(jq -nc --arg e "$build_event" '{event_id:$e,status:"failed",detail:"live executable hash mismatch",anomaly_identity_mismatch:true}')" >/dev/null
  rollback_or_stop 'live executable hash mismatch'
fi
# The candidate is now serving, so the build phase's result is known and the restart and smoke
# phases are recorded against it as their recorded parent.
[ -n "${build_event:-}" ] && record_phase "$(jq -nc --arg e "$build_event" --arg h "$built" '{event_id:$e,status:"succeeded",detail:"release built and live executable hash matches"}')" >/dev/null
restart_event=$(record_phase "$(jq -nc --arg d "$deployment_id" --arg p "${build_event:-}" --arg c "$expected_commit" --arg h "$built" --argjson s "$latest_schema" 'if $p=="" then {deployment_id:$d,phase:"restart",status:"succeeded",commit:$c,binary_sha256:$h,schema_version:$s} else {deployment_id:$d,parent_id:$p,phase:"restart",status:"succeeded",commit:$c,binary_sha256:$h,schema_version:$s} end')")
smoke_event=$(record_phase "$(jq -nc --arg d "$deployment_id" --arg p "${restart_event:-}" 'if $p=="" then {deployment_id:$d,phase:"smoke",status:"started",detail:"authenticated API smoke and malformed-body refusal"} else {deployment_id:$d,parent_id:$p,phase:"smoke",status:"started",detail:"authenticated API smoke and malformed-body refusal"} end')")
if ! curl -fsS -o /dev/null -H "$auth" "$base/scopes"; then
  [ -n "${smoke_event:-}" ] && record_phase "$(jq -nc --arg e "$smoke_event" '{event_id:$e,status:"failed",detail:"authenticated API smoke failed",anomaly_smoke_failed:true}')" >/dev/null
  rollback_or_stop 'authenticated API smoke failed'
fi
refused=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "$auth" -H 'Content-Type: application/json' -d '[]' "$base/chat/submit")
if [ "$refused" != 400 ]; then
  [ -n "${smoke_event:-}" ] && record_phase "$(jq -nc --arg e "$smoke_event" --arg r "$refused" '{event_id:$e,status:"failed",detail:("non-object body returned " + $r),anomaly_smoke_failed:true}')" >/dev/null
  rollback_or_stop "non-object body returned $refused"
fi
[ -n "${smoke_event:-}" ] && record_phase "$(jq -nc --arg e "$smoke_event" '{event_id:$e,status:"succeeded",detail:"API answering and non-object body refused with 400",anomaly_smoke_failed:false,anomaly_unready:false}')" >/dev/null
# The outcome phase closes the trail. Its parent is the smoke phase that accepted the release.
record_phase "$(jq -nc --arg d "$deployment_id" --arg p "${smoke_event:-}" --arg c "$expected_commit" --arg h "$built" --argjson s "$latest_schema" 'if $p=="" then {deployment_id:$d,phase:"outcome",status:"succeeded",commit:$c,binary_sha256:$h,schema_version:$s,detail:"deployment verified"} else {deployment_id:$d,parent_id:$p,phase:"outcome",status:"succeeded",commit:$c,binary_sha256:$h,schema_version:$s,detail:"deployment verified"} end')" >/dev/null

echo "deployed $(git rev-parse --short HEAD) to $unit: pid $pid, release sha256 $built, schema $latest_schema, readiness verified, API answering, non-object body refused with 400, provenance recorded as $deployment_id"
