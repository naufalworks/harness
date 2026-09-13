#!/usr/bin/env python3
"""Schema-aware binary rollback policy and atomic executable restoration."""
from __future__ import annotations

import argparse
import json
import os
import shutil
import stat
import tempfile
from pathlib import Path


def decision(previous_schema: int, current_schema: int) -> dict[str, object]:
    if previous_schema < 0:
        raise ValueError("previous schema version must be non-negative")
    compatible = current_schema >= 0 and current_schema <= previous_schema
    return {
        "action": "restore_previous_binary" if compatible else "database_restore_requires_approval",
        "previous_schema": previous_schema,
        "current_schema": current_schema,
        "binary_rollback_compatible": compatible,
    }


def restore(previous: Path, live: Path) -> None:
    if not previous.is_file():
        raise ValueError(f"previous executable is unavailable: {previous}")
    live.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{live.name}.rollback-", dir=live.parent)
    os.close(fd)
    temporary_path = Path(temporary)
    try:
        shutil.copyfile(previous, temporary_path)
        mode = stat.S_IMODE(previous.stat().st_mode)
        temporary_path.chmod(mode)
        with temporary_path.open("rb") as handle:
            os.fsync(handle.fileno())
        os.replace(temporary_path, live)
        directory = os.open(live.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        temporary_path.unlink(missing_ok=True)


def main() -> None:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    choose = commands.add_parser("decision")
    choose.add_argument("previous_schema", type=int)
    choose.add_argument("current_schema", type=int)
    replace = commands.add_parser("restore-binary")
    replace.add_argument("previous", type=Path)
    replace.add_argument("live", type=Path)
    args = parser.parse_args()
    if args.command == "decision":
        print(json.dumps(decision(args.previous_schema, args.current_schema), sort_keys=True))
    else:
        restore(args.previous, args.live)


if __name__ == "__main__":
    main()
