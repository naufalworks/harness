#!/usr/bin/env python3
"""Deployment safety contract.

Every scenario runs scripts/deploy.sh inside a throwaway fixture repository whose
PATH is prepended with fault-injectable stand-ins for systemctl, curl, cargo and
git. The schema decision itself is NOT stubbed: it is read read-only from a real
SQLite file via the real scripts/deploy_contract.py, so the compatibility logic
under test is production code. No real unit is restarted, no live service is
curled, and no production database is opened.

The suite proves:

* a dirty tracked/untracked tree is refused before any mutation, and production
  still requires the explicit --approve flag;
* every preflight requirement is enforced before stop/rebuild/restart;
* the previous executable is preserved and only restored when the database
  schema (read from the explicit HARNESS_DB path) is proven compatible;
* a schema that moved, or is unknown, refuses automatic rollback, reports
  recovery-required, and leaves the database and user files untouched;
* a stale live process, a missing previous executable and a failed rollback all
  fail closed.
"""
from __future__ import annotations

import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
from deploy_contract import check_health, read_db_schema, schema_compat  # noqa: E402


def sha256_text(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()


class DeploySafetyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name)
        self.root = base / "repo"
        (self.root / "scripts").mkdir(parents=True)
        (self.root / "target" / "release").mkdir(parents=True)
        (self.root / ".env").write_text("HARNESS_AUTH_TOKEN=test-token\nHARNESS_ADDR=127.0.0.1:8080\n")
        shutil.copy2(ROOT / "scripts" / "deploy.sh", self.root / "scripts" / "deploy.sh")
        shutil.copy2(ROOT / "scripts" / "deploy_contract.py", self.root / "scripts" / "deploy_contract.py")
        self.bin = self.root / "target" / "release" / "harness"
        self.previous = self.root / "target" / "release" / "harness.previous"
        self.db = self.root / "data" / "harness_v2.db"
        self.db.parent.mkdir(parents=True)
        self.ctl = base / "ctl"
        self.ctl.mkdir()
        self.bindir = base / "stubbin"
        self.bindir.mkdir()
        self._write_stubs()
        self.env = dict(os.environ)
        self.env["PATH"] = f"{self.bindir}:{self.env['PATH']}"
        self.env["DEPLOY_CTL"] = str(self.ctl)
        self.env["DEPLOY_BIN"] = str(self.bin)
        self.env["HARNESS_DB"] = str(self.db)
        self.env["FAKE_COMMIT"] = "abcd1234"
        self.env["FAKE_EXECSTART"] = f"/usr/lib/systemd/systemd {self.bin}"
        self.env["FAKE_MAINPID"] = "4242"
        self.make_db(7)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    # -- fixture helpers ---------------------------------------------------
    def stub(self, name: str, body: str) -> None:
        path = self.bindir / name
        path.write_text("#!/usr/bin/env bash\n" + body)
        path.chmod(0o755)

    def _write_stubs(self) -> None:
        self.stub("cargo", """
if [ "${FAIL_BUILD:-0}" = 1 ]; then exit 1; fi
mkdir -p "$(dirname "$DEPLOY_BIN")"
printf '%s' "${BUILD_CONTENT:-new-binary}" > "$DEPLOY_BIN"
printf 'call\\n' >> "$DEPLOY_CTL/cargo_calls"
""")
        self.stub("git", """
case "$1 $2" in
  "status --porcelain") cat "$DEPLOY_CTL/git_dirty" 2>/dev/null ;;
  "rev-parse HEAD") echo "${FAKE_COMMIT:-abcd1234}" ;;
  "rev-parse --short") echo "${FAKE_COMMIT:-abcd1234}" ;;
  *) echo "" ;;
esac
""")
        self.stub("systemctl", """
case "$1" in
  show)
    case "$3" in
      ExecStart) echo "${FAKE_EXECSTART:-}" ;;
      MainPID) echo "${FAKE_MAINPID:-4242}" ;;
    esac ;;
  is-active) cat "$DEPLOY_CTL/active" 2>/dev/null || echo active ;;
  restart)
    n=$(( $(cat "$DEPLOY_CTL/restarts" 2>/dev/null || echo 0) + 1 ))
    printf '%s\\n' "$n" > "$DEPLOY_CTL/restarts"
    if [ "$n" -ge 2 ]; then echo rolledback > "$DEPLOY_CTL/stage"; else echo after > "$DEPLOY_CTL/stage"; fi
    if [ -n "${MIGRATE_TO:-}" ] && [ "$n" -eq 1 ] && [ -n "${HARNESS_DB:-}" ]; then
      python3 -c 'import sqlite3,sys;c=sqlite3.connect(sys.argv[1]);c.execute("PRAGMA user_version=%d" % int(sys.argv[2]));c.commit();c.close()' "$HARNESS_DB" "$MIGRATE_TO"
    fi ;;
  status) echo "stub status" ;;
esac
""")
        self.stub("curl", """
a="$*"
if [[ "$a" == *chat/submit* ]]; then echo "${FAKE_SUBMIT_CODE:-400}"; exit 0; fi
if [[ "$a" == *scopes* ]]; then exit 0; fi
if [[ "$a" == *health* ]]; then
  s=$(cat "$DEPLOY_CTL/stage" 2>/dev/null || echo before)
  if [ -f "$DEPLOY_CTL/health_$s.json" ]; then cat "$DEPLOY_CTL/health_$s.json"; exit 0; fi
  if [ -f "$DEPLOY_CTL/health.json" ]; then cat "$DEPLOY_CTL/health.json"; exit 0; fi
  exit 1
fi
exit 0
""")
        # sha256sum is real, except that /proc/PID/exe is mapped to the fixture artifact so
        # the live-process identity check can be exercised without a real process. A stale
        # process is injected with FAKE_STALE_PROC until the first rollback restart.
        self.stub("sha256sum", """
f="$1"
if [[ "$f" == /proc/* ]]; then
  n=$(cat "$DEPLOY_CTL/restarts" 2>/dev/null || echo 0)
  if [ "${FAKE_STALE_PROC:-0}" = 1 ] && [ "$n" -lt 2 ]; then f="$DEPLOY_CTL/running.exe"; else f="$DEPLOY_BIN"; fi
fi
python3 -c 'import hashlib,sys;print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$f"
""")
        # Fault injection only: a real cp, made to fail exactly on the rollback copy.
        self.stub("cp", """
if [ "${FAIL_ROLLBACK:-0}" = 1 ]; then
  case "$1" in *harness.previous*) echo "simulated rollback copy failure" >&2; exit 1 ;; esac
fi
command -p cp "$@"
""")

    def make_db(self, version: int) -> None:
        connection = sqlite3.connect(str(self.db))
        connection.execute(f"PRAGMA user_version={version}")
        connection.commit()
        connection.close()

    def db_version(self) -> int:
        connection = sqlite3.connect(f"file:{self.db}?mode=ro", uri=True)
        try:
            return connection.execute("PRAGMA user_version").fetchone()[0]
        finally:
            connection.close()

    def write_health(self, stage: str, *, ready: bool = True, schema: int = 7, sha: str = "", workers: bool = True) -> None:
        doc = {
            "ready": ready,
            "commit": "abcd1234",
            "binary_sha256": sha,
            "schema_version": schema,
            "started_at": "2026-09-13T00:00:00Z",
            "port": 8080,
            "database": {"ready": True},
            "workers": {"recording": workers, "extraction": workers},
        }
        (self.ctl / f"health_{stage}.json").write_text(json.dumps(doc))

    def run_deploy(self, *args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess:
        merged = dict(self.env)
        if env:
            merged.update(env)
        return subprocess.run(
            ["bash", str(self.root / "scripts" / "deploy.sh"), *args],
            capture_output=True,
            text=True,
            env=merged,
            cwd=str(self.root),
        )

    def restarts(self) -> str:
        path = self.ctl / "restarts"
        return path.read_text().strip() if path.exists() else "0"

    def assert_no_mutation(self) -> None:
        self.assertFalse((self.ctl / "cargo_calls").exists(), "build must not run before the gate")
        self.assertFalse((self.ctl / "restarts").exists(), "restart must not run before the gate")

    # -- tests -------------------------------------------------------------
    def test_clean_approved_deploy_verifies_readiness_and_running_binary(self) -> None:
        self.bin.write_text("new-binary")
        self.write_health("after", schema=7, sha=sha256_text("new-binary"))
        result = self.run_deploy("--approve", "--unit", "stub-unit")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("deployed", result.stdout)
        self.assertIn("readiness verified", result.stdout)
        self.assertEqual(self.restarts(), "1")

    def test_dirty_tree_is_refused_before_any_mutation(self) -> None:
        (self.ctl / "git_dirty").write_text(" M src/main.rs\n?? scratch.txt\n")
        self.bin.write_text("old-binary")
        result = self.run_deploy("--approve", "--unit", "stub-unit")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("REFUSED", result.stderr)
        self.assertEqual(self.bin.read_text(), "old-binary")
        self.assert_no_mutation()

    def test_allow_dirty_is_an_explicit_attestation(self) -> None:
        (self.ctl / "git_dirty").write_text("?? scratch.txt\n")
        self.bin.write_text("new-binary")
        self.write_health("after", schema=7, sha=sha256_text("new-binary"))
        result = self.run_deploy("--approve", "--allow-dirty", "--unit", "stub-unit")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("dirty", result.stderr)

    def test_approval_flag_is_required_before_any_mutation(self) -> None:
        self.bin.write_text("old-binary")
        result = self.run_deploy("--unit", "stub-unit")
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertIn("APPROVAL REQUIRED", result.stderr)
        self.assertEqual(self.bin.read_text(), "old-binary")
        self.assertFalse(self.previous.exists())
        self.assert_no_mutation()

    def test_preflight_unit_mismatch_blocks_before_mutation(self) -> None:
        self.bin.write_text("old-binary")
        result = self.run_deploy("--approve", "--unit", "stub-unit", env={"FAKE_EXECSTART": "/other/harness"})
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("BLOCKED", result.stderr)
        self.assertEqual(self.bin.read_text(), "old-binary")
        self.assertFalse(self.previous.exists())
        self.assert_no_mutation()

    def test_preflight_missing_env_blocks_before_mutation(self) -> None:
        (self.root / ".env").unlink()
        self.bin.write_text("old-binary")
        result = self.run_deploy("--approve", "--unit", "stub-unit")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("BLOCKED", result.stderr)
        self.assert_no_mutation()

    def test_failed_readiness_rolls_back_when_schema_unchanged(self) -> None:
        self.bin.write_text("old-binary")
        self.write_health("after", ready=False, schema=7, sha=sha256_text("new-binary"))
        result = self.run_deploy("--approve", "--unit", "stub-unit")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("rolled back", result.stderr)
        self.assertIn("previous executable", result.stderr)
        self.assertEqual(self.restarts(), "2")
        self.assertEqual(self.bin.read_text(), "old-binary")
        self.assertEqual(self.db_version(), 7)

    def test_stale_live_process_is_detected_and_rolled_back(self) -> None:
        self.bin.write_text("old-binary")
        (self.ctl / "running.exe").write_text("stale-binary")
        self.write_health("after", schema=7, sha=sha256_text("new-binary"))
        result = self.run_deploy("--approve", "--unit", "stub-unit", env={"FAKE_STALE_PROC": "1"})
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("does not match the binary just built", result.stderr)
        self.assertIn("rolled back", result.stderr)
        self.assertEqual(self.restarts(), "2")

    def test_schema_change_refuses_automatic_rollback(self) -> None:
        self.bin.write_text("old-binary")
        self.write_health("after", ready=False, schema=8, sha=sha256_text("new-binary"))
        result = self.run_deploy("--approve", "--unit", "stub-unit", env={"MIGRATE_TO": "8"})
        self.assertEqual(result.returncode, 4, result.stderr)
        self.assertIn("RECOVERY REQUIRED", result.stderr)
        self.assertIn("refused", result.stderr)
        self.assertEqual(self.bin.read_text(), "new-binary")
        self.assertEqual(self.restarts(), "1")
        # The migration the new binary performed is preserved; nothing was reset or restored.
        self.assertEqual(self.db_version(), 8)

    def test_declared_schema_range_permits_a_scoped_rollback(self) -> None:
        self.make_db(6)
        self.bin.write_text("old-binary")
        self.write_health("after", ready=False, schema=7, sha=sha256_text("new-binary"))
        result = self.run_deploy(
            "--approve", "--unit", "stub-unit", "--schema-range", "6,7", env={"MIGRATE_TO": "7"}
        )
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("rolled back", result.stderr)

    def test_unknown_schema_refuses_rollback(self) -> None:
        # A readable but non-SQLite file: preflight passes, the schema is unknowable.
        self.db.write_text("this is not a sqlite database")
        self.bin.write_text("old-binary")
        self.write_health("after", ready=False, schema=7, sha=sha256_text("new-binary"))
        result = self.run_deploy("--approve", "--unit", "stub-unit")
        self.assertEqual(result.returncode, 4, result.stderr)
        self.assertIn("RECOVERY REQUIRED", result.stderr)
        self.assertIn("unknown", result.stderr)

    def test_missing_previous_executable_reports_recovery_required(self) -> None:
        self.write_health("after", ready=False, schema=7, sha=sha256_text("new-binary"))
        result = self.run_deploy("--approve", "--unit", "stub-unit")
        self.assertEqual(result.returncode, 4, result.stderr)
        self.assertIn("RECOVERY REQUIRED", result.stderr)
        self.assertIn("no previous executable", result.stderr)
        self.assertEqual(self.restarts(), "1")

    def test_failed_rollback_reports_recovery_required(self) -> None:
        self.bin.write_text("old-binary")
        self.write_health("after", ready=False, schema=7, sha=sha256_text("new-binary"))
        result = self.run_deploy("--approve", "--unit", "stub-unit", env={"FAIL_ROLLBACK": "1"})
        self.assertEqual(result.returncode, 4, result.stderr)
        self.assertIn("ROLLBACK FAILED", result.stderr)
        self.assertEqual(self.restarts(), "1")


class SchemaContractTests(unittest.TestCase):
    def test_unchanged_schema_allows_rollback(self) -> None:
        allowed, reason = schema_compat(7, 7, "")
        self.assertTrue(allowed, reason)

    def test_forward_schema_change_refuses(self) -> None:
        allowed, reason = schema_compat(7, 8, "")
        self.assertFalse(allowed)
        self.assertIn("not established", reason)

    def test_unknown_schema_fails_closed(self) -> None:
        self.assertFalse(schema_compat(None, 7, "")[0])
        self.assertFalse(schema_compat(7, "unknown", "")[0])
        self.assertFalse(schema_compat("", "", "")[0])

    def test_declared_range_allows_explicit_compatibility(self) -> None:
        self.assertTrue(schema_compat(6, 7, "6,7")[0])
        self.assertFalse(schema_compat(6, 8, "6,7")[0])

    def test_check_health_detects_identity_and_component_mismatch(self) -> None:
        doc = {
            "ready": True,
            "commit": "a",
            "binary_sha256": "b",
            "schema_version": 7,
            "database": {"ready": True},
            "workers": {"recording": True, "extraction": True},
        }
        self.assertEqual(check_health(doc, "a", "b", "7"), [])
        self.assertTrue(check_health(doc, "a", "c", "7"))
        self.assertTrue(check_health(doc, "a", "b", "8"))
        self.assertTrue(check_health({}, "a", "b", "7"))

    def test_read_db_schema_reads_user_version_read_only(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "harness.db"
            connection = sqlite3.connect(str(path))
            connection.execute("PRAGMA user_version=5")
            connection.commit()
            connection.close()
            self.assertEqual(read_db_schema(str(path)), 5)
            self.assertIsNone(read_db_schema(str(Path(tmp) / "missing.db")))
            junk = Path(tmp) / "junk.db"
            junk.write_text("not a database")
            self.assertIsNone(read_db_schema(str(junk)))
            self.assertIsNone(read_db_schema(None))


if __name__ == "__main__":
    unittest.main()
