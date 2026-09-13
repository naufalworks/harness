#!/usr/bin/env python3
"""Disposable rollback policy fixture; never touches systemd or production data."""
from __future__ import annotations

import json
import subprocess
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
POLICY = ROOT / "scripts/rollback_policy.py"


def run(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(["python3", str(POLICY), *args], text=True, capture_output=True, check=True)


def main() -> None:
    compatible = json.loads(run("decision", "7", "7").stdout)
    assert compatible["action"] == "restore_previous_binary"
    assert compatible["binary_rollback_compatible"] is True

    with tempfile.TemporaryDirectory(prefix="harness-deploy-rollback-") as temporary:
        root = Path(temporary)
        previous = root / "previous-harness"
        live = root / "harness"
        previous.write_text("#!/bin/sh\nprintf 'previous-serving\\n'\n")
        previous.chmod(0o700)
        live.write_text("#!/bin/sh\nprintf 'broken-candidate\\n'\n")
        live.chmod(0o700)
        run("restore-binary", str(previous), str(live))
        served = subprocess.run([str(live)], text=True, capture_output=True, check=True)
        assert served.stdout == "previous-serving\n"
        assert live.stat().st_mode & 0o777 == 0o700

        live.write_text("#!/bin/sh\nprintf 'new-schema-candidate\\n'\n")
        incompatible = json.loads(run("decision", "7", "8").stdout)
        assert incompatible["action"] == "database_restore_requires_approval"
        assert incompatible["binary_rollback_compatible"] is False
        unknown = json.loads(run("decision", "7", "-1").stdout)
        assert unknown["action"] == "database_restore_requires_approval"
        assert subprocess.run([str(live)], text=True, capture_output=True, check=True).stdout == "new-schema-candidate\n"

    print("Deploy rollback fixture passed: compatible binary restored atomically; newer schema stopped for approved recovery.")


if __name__ == "__main__":
    main()
