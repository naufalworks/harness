#!/usr/bin/env python3
"""Release gate truthfulness.

scripts/verify_release.sh is exercised inside a throwaway repository whose PATH
is prepended with stub cargo/node binaries and a fake Chromium, so no toolchain
build runs. The suite proves:

* the strict gate actually runs the real E2E lane (browser_e2e.cjs) as well as
  the mocked browser suites, and reports an explicit pass only when nothing is
  skipped;
* a missing browser runtime blocks the strict gate instead of silently skipping;
* --dev may skip the browser/E2E lanes but its final line never claims a release
  pass, and each skipped suite is labelled;
* a failing suite propagates to a non-zero exit and a FAILED summary;
* the gate never invokes the production deployment path.
"""
from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class ReleaseGateTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(self.tmp.name)
        self.root = base / "repo"
        (self.root / "scripts").mkdir(parents=True)
        for name in ("verify_release.sh", "verify_browser.sh", "verify_e2e.sh"):
            target = self.root / "scripts" / name
            shutil.copy2(ROOT / "scripts" / name, target)
            target.chmod(0o755)
        (self.root / "static").mkdir()
        (self.root / "static" / "app.js").write_text("// fixture\n")
        tests = self.root / "tests"
        tests.mkdir()
        (tests / "test_fixture.py").write_text(
            "import unittest\n\n\nclass T(unittest.TestCase):\n    def test_ok(self):\n        self.assertTrue(True)\n"
        )
        (tests / "integration_smoke.py").write_text("print('smoke ok')\n")
        (tests / "recording_integration.py").write_text("print('recording ok')\n")

        self.bindir = base / "stubbin"
        self.bindir.mkdir()
        self.node_log = base / "node.log"
        self._write_stubs()

        self.chromium = base / "chromium"
        self.chromium.write_text("#!/usr/bin/env bash\nexit 0\n")
        self.chromium.chmod(0o755)

        self.env = dict(os.environ)
        self.env["PATH"] = f"{self.bindir}:{self.env['PATH']}"
        self.env["STUB_LOG"] = str(self.node_log)
        self.env["CHROMIUM_PATH"] = str(self.chromium)

    def tearDown(self) -> None:
        self.tmp.cleanup()

    def _write_stubs(self) -> None:
        cargo = self.bindir / "cargo"
        cargo.write_text("#!/usr/bin/env bash\nexit ${FAIL_CARGO:-0}\n")
        cargo.chmod(0o755)
        node = self.bindir / "node"
        node.write_text(
            "#!/usr/bin/env bash\n"
            'echo "node $*" >> "$STUB_LOG"\n'
            'case " $* " in\n'
            '  *browser_e2e.cjs*) [ "${FAIL_E2E:-0}" = 1 ] && exit 1 ;;\n'
            '  *ui_smoke.cjs*)    [ "${FAIL_UI:-0}" = 1 ] && exit 1 ;;\n'
            "esac\n"
            "exit 0\n"
        )
        node.chmod(0o755)

    def with_node_modules(self) -> None:
        (self.root / "node_modules").mkdir(exist_ok=True)

    def run_gate(self, *args: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess:
        merged = dict(self.env)
        if env:
            merged.update(env)
        return subprocess.run(
            ["bash", str(self.root / "scripts" / "verify_release.sh"), *args],
            capture_output=True,
            text=True,
            env=merged,
            cwd=str(self.root),
        )

    def test_strict_gate_runs_mocked_browser_and_real_e2e(self) -> None:
        self.with_node_modules()
        result = self.run_gate()
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, out)
        self.assertIn("PASSED", out)
        self.assertIn("0 skipped", out)
        self.assertNotIn("[SKIP]", out)
        log = self.node_log.read_text()
        self.assertIn("tests/browser_e2e.cjs", log)
        self.assertIn("tests/browser_e2e_failure.cjs", log)
        self.assertIn("tests/recording_ui.cjs", log)
        self.assertIn("tests/ui_smoke.cjs", log)

    def test_strict_gate_blocks_when_browser_dependencies_missing(self) -> None:
        result = self.run_gate()
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 2, out)
        self.assertIn("BLOCKED", out)
        self.assertNotIn("PASSED", out)
        log = self.node_log.read_text() if self.node_log.exists() else ""
        self.assertNotIn("browser_e2e.cjs", log, "the E2E lane must not run when the browser runtime is missing")

    def test_dev_gate_skips_explicitly_and_never_claims_a_release_pass(self) -> None:
        result = self.run_gate("--dev")
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 0, out)
        self.assertIn("[SKIP] browser-mocked", out)
        self.assertIn("[SKIP] browser-e2e", out)
        self.assertIn("NOT a release gate pass", out)
        self.assertNotIn("PASSED", out)

    def test_failing_suite_propagates_and_fails_the_gate(self) -> None:
        self.with_node_modules()
        result = self.run_gate(env={"FAIL_E2E": "1"})
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 1, out)
        self.assertIn("[FAIL] browser-e2e", out)
        self.assertIn("FAILED", out)
        self.assertNotIn("0 skipped", out)

    def test_dev_gate_still_fails_a_running_suite(self) -> None:
        self.with_node_modules()
        result = self.run_gate("--dev", env={"FAIL_UI": "1"})
        out = result.stdout + result.stderr
        self.assertEqual(result.returncode, 1, out)
        self.assertIn("[FAIL] browser-mocked", out)

    def test_gate_never_invokes_deployment(self) -> None:
        self.with_node_modules()
        result = self.run_gate()
        out = result.stdout + result.stderr
        self.assertNotIn("systemctl", out)
        self.assertNotIn("deploy.sh", out)


if __name__ == "__main__":
    unittest.main()
