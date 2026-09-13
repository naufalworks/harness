#!/usr/bin/env bash
# Deploy the harness to its systemd unit, then prove what is actually running.
#
# Why this exists: the unit starts target/release/harness, while scripts/verify_release.sh
# builds and tests the *debug* profile. Nothing else in the repo rebuilds the artifact systemd
# starts, so `systemctl restart` can faithfully restart a binary older than the change being
# deployed. That happened on 2026-09-12: a pushed, fully gated fix was simply absent from the
# live service, and the restart looked successful. Gate first (scripts/verify_release.sh), then
# deploy with this.
#
# Safety contract:
#   * refuses a dirty tracked/untracked working tree unless --allow-dirty attests it;
#   * makes no mutation (stop/rebuild/restart) without the explicit --approve flag;
#   * runs every preflight check before the first mutation;
#   * preserves the previous executable and only restores it when the database schema — read
#     read-only from the explicit HARNESS_DB path, not from the service's own health claim —
#     is proven compatible; otherwise it reports RECOVERY REQUIRED and never resets, restores
#     or deletes the database or any user files.
#
# The database path is taken from the HARNESS_DB environment variable only (never from .env),
# so the schema decision cannot be redirected by arbitrary configuration. If it is missing or
# unreadable the schema is unknown and a failed deployment requires recovery.
#
# Usage: bash scripts/deploy.sh --approve [--unit NAME] [--allow-dirty] [--schema-range RANGE]
# Exit codes:
#   0 deployed and readiness/smoke verified
#   1 deployment failed and the previous executable was rolled back and is serving
#   2 refused/blocked (dirty tree, missing preflight requirement, unit mismatch)
#   3 approval flag missing (nothing was mutated)
#   4 recovery required (rollback refused, impossible, or itself failed)
set -euo pipefail

cd "$(dirname "$0")/.." || exit 2
root=$PWD
helper=$root/scripts/deploy_contract.py
unit=${HARNESS_UNIT:-harness}
# DB selection comes from the environment only, before anything else is sourced.
db_path=${HARNESS_DB:-}
approve=0
allow_dirty=0
schema_range=${HARNESS_ROLLBACK_SCHEMA_RANGE:-}
expected_schema=${HARNESS_EXPECTED_SCHEMA:-7}

while [ $# -gt 0 ]; do
  case $1 in
    --approve) approve=1 ;;
    --unit) unit=$2; shift ;;
    --unit=*) unit=${1#*=} ;;
    --allow-dirty) allow_dirty=1 ;;
    --schema-range) schema_range=$2; shift ;;
    --schema-range=*) schema_range=${1#*=} ;;
    -h|--help) sed -n '2,34p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

bin=$root/target/release/harness
previous=$root/target/release/harness.previous

# 1. Dirty working tree is refused by default: a correct binary hash can still hide source
#    that no commit describes. --allow-dirty is the explicit attestation.
dirty=$(git status --porcelain 2>/dev/null || true)
if [ -n "$dirty" ]; then
  if [ "$allow_dirty" -ne 1 ]; then
    echo "REFUSED: working tree is dirty; commit, stash, or pass --allow-dirty to attest the extra source." >&2
    printf '%s\n' "$dirty" >&2
    exit 2
  fi
  echo "WARNING: deploying with a dirty tree under --allow-dirty; the embedded commit does not describe every source change." >&2
fi

# 2. Approval gate: no mutation without an explicit, separate authorization.
if [ "$approve" -ne 1 ]; then
  echo "APPROVAL REQUIRED: this command stops, rebuilds and restarts systemd unit '$unit'." >&2
  echo "Re-run with --approve to authorize: bash scripts/deploy.sh --approve --unit $unit" >&2
  exit 3
fi

# 3. Preflight — every requirement checked before the first mutation.
for tool in python3 cargo curl; do
  command -v "$tool" >/dev/null 2>&1 || { echo "BLOCKED: $tool is required." >&2; exit 2; }
done
[ -f "$helper" ] || { echo "BLOCKED: missing deployment contract helper $helper" >&2; exit 2; }

exec_start=$(systemctl show -p ExecStart --value "$unit" 2>/dev/null || true)
case $exec_start in
  *"$bin"*) ;;
  *)
    echo "BLOCKED: $unit does not start $bin (ExecStart: ${exec_start:-<none>})" >&2
    exit 2
    ;;
esac

[ -f "$root/.env" ] || { echo "BLOCKED: $root/.env is required for the readiness/smoke checks." >&2; exit 2; }
if [ -n "$db_path" ] && [ ! -r "$db_path" ]; then
  echo "BLOCKED: HARNESS_DB=$db_path is not a readable file." >&2
  exit 2
fi
if [ -z "$db_path" ]; then
  echo "WARNING: HARNESS_DB is not set; schema compatibility cannot be established and a failed deployment will require recovery." >&2
fi

set -a
. "$root/.env"
set +a
if [ -z "${HARNESS_AUTH_TOKEN:-}" ]; then
  echo "BLOCKED: HARNESS_AUTH_TOKEN is missing from .env" >&2
  exit 2
fi
base="http://${HARNESS_ADDR:-127.0.0.1:8080}"
auth="Authorization: Bearer ${HARNESS_AUTH_TOKEN}"
expected_commit=$(git rev-parse HEAD)

previous_available=0
[ -f "$bin" ] && previous_available=1

# --- mutations begin here -------------------------------------------------

read_db_schema() {
  python3 "$helper" read-db-schema --db "$db_path" 2>/dev/null || echo unknown
}

if [ "$previous_available" -eq 1 ]; then
  cp -p "$bin" "$previous" || { echo "BLOCKED: could not preserve the previous executable." >&2; exit 2; }
fi
sha_previous=''
[ "$previous_available" -eq 1 ] && sha_previous=$(sha256sum "$previous" | cut -d' ' -f1)

# The schema the old binary is actually serving, read read-only from the explicit DB path.
schema_before=$(read_db_schema)

echo "building the release profile the unit runs"
cargo build --locked --release
built=$(sha256sum "$bin" | cut -d' ' -f1)

wait_active() {
  local _
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    sleep 1
    [ "$(systemctl is-active "$unit" 2>/dev/null || true)" = active ] && return 0
  done
  return 1
}

verify_running_sha() {
  local pid running
  pid=$(systemctl show -p MainPID --value "$unit" 2>/dev/null || true)
  [ -n "$pid" ] || return 1
  running=$(sha256sum "/proc/$pid/exe" 2>/dev/null | cut -d' ' -f1 || true)
  [ -n "$running" ] || return 1
  [ "$running" = "$1" ]
}

rollback_or_recover() {
  local reason=$1
  local schema_after=$2
  echo "FAILED: $reason" >&2

  if [ "$previous_available" -ne 1 ]; then
    echo "RECOVERY REQUIRED: no previous executable was preserved; the new binary is running and the database was not reset or restored. Fix forward or restore from an approved backup." >&2
    exit 4
  fi

  local decision
  decision=$(python3 "$helper" schema-compat --before "$schema_before" --after "$schema_after" --declared-range "$schema_range" 2>/dev/null || true)
  case $decision in
    ALLOW:*) echo "binary rollback permitted: ${decision#ALLOW: }" >&2 ;;
    *)
      echo "RECOVERY REQUIRED: automatic binary rollback refused (${decision#REFUSE: }). Schema before=$schema_before after=$schema_after. Do not run the previous binary against this database until an approved recovery procedure is chosen; the database was not reset or restored." >&2
      exit 4
      ;;
  esac

  echo "restoring the previous executable" >&2
  if ! cp "$previous" "$bin"; then
    echo "ROLLBACK FAILED: could not restore $previous to $bin; recovery required." >&2
    exit 4
  fi
  systemctl restart "$unit" || true
  if ! wait_active; then
    echo "ROLLBACK FAILED: $unit did not become active after restoring the previous executable." >&2
    exit 4
  fi
  if ! verify_running_sha "$sha_previous"; then
    echo "ROLLBACK FAILED: the running process does not match the preserved executable ($sha_previous)." >&2
    exit 4
  fi
  echo "deployment failed and was rolled back: $unit is serving the previous executable ($sha_previous); the database was not reset or restored." >&2
  exit 1
}

systemctl restart "$unit"
if ! wait_active; then
  systemctl status "$unit" --no-pager | tail -20 >&2 || true
  rollback_or_recover "$unit is not active" "$(read_db_schema)"
fi

# The schema after the new binary's startup migrations, read read-only from the explicit DB.
schema_after=$(read_db_schema)

# The check that was missing: the live process must be the binary this run produced.
if ! verify_running_sha "$built"; then
  rollback_or_recover "the live process does not match the binary just built ($built)" "$schema_after"
fi

pid=$(systemctl show -p MainPID --value "$unit" 2>/dev/null || true)

# Smoke test: the API answers, and a non-object body is still refused. Neither request writes.
health=''
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do
  if health=$(curl -fsS -H "$auth" "$base/health" 2>/dev/null); then break; fi
  sleep 1
done
[ -n "$health" ] || rollback_or_recover 'readiness endpoint did not become ready' "$(read_db_schema)"

if ! problems=$(printf '%s' "$health" | python3 "$helper" check-health --expected-commit "$expected_commit" --expected-sha "$built" --expected-schema "$schema_after" 2>&1); then
  rollback_or_recover "readiness check failed: $problems" "$schema_after"
fi

curl -fsS -o /dev/null -H "$auth" "$base/scopes" || rollback_or_recover 'the /scopes endpoint did not answer' "$schema_after"
refused=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "$auth" -H 'Content-Type: application/json' -d '[]' "$base/chat/submit")
if [ "$refused" != 400 ]; then
  rollback_or_recover "a non-object body returned $refused, expected 400" "$schema_after"
fi

echo "deployed $(git rev-parse --short HEAD) to $unit: pid $pid, release sha256 $built, readiness verified, schema $schema_after, API answering, non-object body refused with 400"
