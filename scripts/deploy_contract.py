#!/usr/bin/env python3
"""Deployment contract helpers for scripts/deploy.sh.

Pure, dependency-free helpers invoked from the shell so the readiness and
rollback decisions are explicit and testable instead of inline jq expressions:

* check-health   — validate a readiness/health JSON document on stdin.
* read-schema    — print the ``schema_version`` from a health document, or
                   ``unknown`` when it is absent or not an integer.
* read-db-schema — read ``PRAGMA user_version`` from an explicit SQLite database
                   file, opened read-only. Never writes, migrates or locks the
                   live database for writing.
* schema-compat  — decide whether an automatic binary rollback is safe given the
                   database schema observed before and after a failed upgrade.

Every decision fails closed: unknown, missing or malformed input is never
treated as success, and a schema change without an explicitly declared
compatible range is never considered safe to roll back.
"""
from __future__ import annotations

import argparse
import json
import os
import sqlite3
import sys
from typing import Any, Iterable


def _as_int(value: Any) -> int | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, str):
        text = value.strip()
        if text.lstrip("-").isdigit():
            return int(text)
    return None


def check_health(doc: Any, expected_commit: str, expected_sha: str, expected_schema: str | None) -> list[str]:
    """Return a list of readiness problems; an empty list means healthy."""
    if not isinstance(doc, dict):
        return ["health payload is not a JSON object"]

    def get(*path: str, default: Any = None) -> Any:
        node: Any = doc
        for key in path:
            if not isinstance(node, dict) or key not in node:
                return default
            node = node[key]
        return node

    problems: list[str] = []
    if get("ready") is not True:
        problems.append("ready is not true")

    commit = get("commit")
    if commit != expected_commit:
        problems.append(f"commit {commit!r} != expected {expected_commit!r}")

    sha = get("binary_sha256")
    if sha != expected_sha:
        problems.append(f"binary_sha256 {sha!r} != built {expected_sha!r}")

    schema = _as_int(get("schema_version"))
    expected = _as_int(expected_schema)
    if schema is None:
        problems.append("schema_version is missing or not an integer")
    elif expected is not None and schema != expected:
        problems.append(f"schema_version {schema} != expected {expected}")

    if get("database", "ready") is not True:
        problems.append("database.ready is not true")
    for worker in ("recording", "extraction"):
        if get("workers", worker) is not True:
            problems.append(f"workers.{worker} is not true")
    return problems


def read_db_schema(path: str | None) -> int | None:
    """Read ``PRAGMA user_version`` from an explicit SQLite file, read-only.

    Returns ``None`` for a missing path, an unreadable file, or anything that is
    not a SQLite database. This never opens the database for writing.
    """
    if not path:
        return None
    real = os.path.abspath(path)
    if not os.path.isfile(real):
        return None
    try:
        connection = sqlite3.connect(f"file:{real}?mode=ro", uri=True)
    except sqlite3.Error:
        return None
    try:
        row = connection.execute("PRAGMA user_version").fetchone()
    except sqlite3.Error:
        return None
    finally:
        connection.close()
    return _as_int(row[0]) if row else None


def _parse_range(spec: str | None) -> set[int] | None:
    if not spec:
        return None
    values: set[int] = set()
    for chunk in spec.replace(" ", "").split(","):
        if not chunk:
            continue
        parsed = _as_int(chunk)
        if parsed is None:
            return None
        values.add(parsed)
    return values or None


def schema_compat(before: Any, after: Any, declared_range: str | None = None) -> tuple[bool, str]:
    """Return ``(allowed, reason)`` for restoring the previous binary.

    A rollback is only safe when the schema did not move, or when both observed
    versions sit inside an explicitly declared compatible range. Anything
    unknown refuses.
    """
    old = _as_int(before)
    new = _as_int(after)
    if old is None or new is None:
        return False, f"schema version unknown (before={before!r}, after={after!r})"
    if old == new:
        return True, f"schema unchanged at version {new}"
    allowed = _parse_range(declared_range)
    if allowed is not None and old in allowed and new in allowed:
        return True, f"schema move {old}->{new} inside declared compatible range {declared_range}"
    return False, f"schema moved {old}->{new}; previous-binary compatibility is not established"


def _cmd_check_health(args: argparse.Namespace) -> int:
    raw = sys.stdin.read()
    try:
        doc = json.loads(raw)
    except (ValueError, TypeError) as error:
        print(f"FAIL: health payload is not JSON: {error}")
        return 1
    problems = check_health(doc, args.expected_commit, args.expected_sha, args.expected_schema or None)
    if problems:
        for problem in problems:
            print(f"FAIL: {problem}")
        return 1
    print("OK: readiness verified")
    return 0


def _cmd_read_schema(_: argparse.Namespace) -> int:
    raw = sys.stdin.read()
    try:
        doc = json.loads(raw)
    except (ValueError, TypeError):
        print("unknown")
        return 0
    schema = _as_int(doc.get("schema_version")) if isinstance(doc, dict) else None
    print(schema if schema is not None else "unknown")
    return 0


def _cmd_read_db_schema(args: argparse.Namespace) -> int:
    schema = read_db_schema(args.db)
    print(schema if schema is not None else "unknown")
    return 0


def _cmd_schema_compat(args: argparse.Namespace) -> int:
    allowed, reason = schema_compat(args.before, args.after, args.declared_range)
    print(("ALLOW: " if allowed else "REFUSE: ") + reason)
    return 0 if allowed else 3


def main(argv: Iterable[str]) -> int:
    parser = argparse.ArgumentParser(description="Deployment contract checks for scripts/deploy.sh")
    sub = parser.add_subparsers(dest="command", required=True)

    health = sub.add_parser("check-health", help="validate a readiness JSON document on stdin")
    health.add_argument("--expected-commit", required=True)
    health.add_argument("--expected-sha", required=True)
    health.add_argument("--expected-schema", default="")
    health.set_defaults(func=_cmd_check_health)

    read = sub.add_parser("read-schema", help="print schema_version from stdin, else unknown")
    read.set_defaults(func=_cmd_read_schema)

    db = sub.add_parser("read-db-schema", help="read PRAGMA user_version from an explicit DB, read-only")
    db.add_argument("--db", default="")
    db.set_defaults(func=_cmd_read_db_schema)

    compat = sub.add_parser("schema-compat", help="decide whether a binary rollback is safe")
    compat.add_argument("--before", required=True)
    compat.add_argument("--after", required=True)
    compat.add_argument("--declared-range", default="")
    compat.set_defaults(func=_cmd_schema_compat)

    args = parser.parse_args(list(argv))
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
