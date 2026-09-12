#!/usr/bin/env bash
# Deploy the harness to its systemd unit, then prove what is actually running.
#
# Why this exists: the unit starts target/release/harness, while scripts/verify_release.sh builds
# and tests the *debug* profile. Nothing else in the repo rebuilds the artifact systemd starts, so
# `systemctl restart` can faithfully restart a binary older than the change being deployed. That
# happened on 2026-09-12: a pushed, fully gated fix was simply absent from the live service, and
# the restart looked successful. Gate first (scripts/verify_release.sh), then deploy with this.
#
# Usage: bash scripts/deploy.sh
# Env:   HARNESS_UNIT (default: harness)
set -euo pipefail

cd "$(dirname "$0")/.."
unit=${HARNESS_UNIT:-harness}
bin=$PWD/target/release/harness

dirty=$(git status --porcelain 2>/dev/null | wc -l)
if [ "$dirty" -gt 0 ]; then
	echo "WARNING: working tree has $dirty uncommitted change(s); deploying it anyway" >&2
fi

# Refuse to "deploy" by building something the unit does not start.
exec_start=$(systemctl show -p ExecStart --value "$unit")
case $exec_start in
*"$bin"*) ;;
*)
	echo "BLOCKED: $unit does not start $bin (ExecStart: $exec_start)" >&2
	exit 2
	;;
esac

echo "building the release profile the unit runs"
cargo build --locked --release
built=$(md5sum "$bin" | cut -d' ' -f1)

systemctl restart "$unit"
for _ in 1 2 3 4 5 6 7 8 9 10; do
	sleep 1
	[ "$(systemctl is-active "$unit")" = active ] && break
done
if [ "$(systemctl is-active "$unit")" != active ]; then
	systemctl status "$unit" --no-pager | tail -20 >&2
	echo "FAILED: $unit is not active" >&2
	exit 1
fi

# The check that was missing: the live process must be the binary this run produced.
pid=$(systemctl show -p MainPID --value "$unit")
running=$(md5sum "/proc/$pid/exe" | cut -d' ' -f1)
if [ "$built" != "$running" ]; then
	echo "FAILED: pid $pid runs md5 $running, not the binary just built ($built)" >&2
	exit 1
fi

# Smoke test: the API answers, and a non-object body is still refused. Neither request writes.
set -a
. ./.env
set +a
base="http://${HARNESS_ADDR:-127.0.0.1:8080}"
auth="Authorization: Bearer ${HARNESS_AUTH_TOKEN:?HARNESS_AUTH_TOKEN missing from .env}"
curl -fsS -o /dev/null -H "$auth" "$base/scopes"
refused=$(curl -s -o /dev/null -w '%{http_code}' -X POST -H "$auth" -H 'Content-Type: application/json' -d '[]' "$base/chat/submit")
if [ "$refused" != 400 ]; then
	echo "FAILED: a non-object body returned $refused, expected 400" >&2
	exit 1
fi

echo "deployed $(git rev-parse --short HEAD) to $unit: pid $pid, release md5 $built, API answering, non-object body refused with 400"
