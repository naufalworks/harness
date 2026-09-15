#!/usr/bin/env python3
"""Fail-closed project freezer for Memory Wind Tunnel capsules.

The Rust treatment contract owns treatment and memory identities. This deliberately small helper
performs only the git boundary: compute the raw tracked-tree SHA-256 or create one detached clean
worktree after checking the source before and after. It never opens the Harness database.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import subprocess
import sys
from pathlib import Path


def git(repo: Path, *args: str) -> bytes:
    env = os.environ.copy()
    env.pop("GIT_CONFIG_GLOBAL", None)
    env.pop("GIT_CONFIG_SYSTEM", None)
    result = subprocess.run(
        ["git", "-C", str(repo), *args],
        check=False,
        capture_output=True,
        env=env,
    )
    if result.returncode:
        raise RuntimeError(
            f"git {' '.join(args)} failed: {result.stderr.decode(errors='replace').strip()}"
        )
    return result.stdout


def head(repo: Path) -> str:
    return git(repo, "rev-parse", "HEAD").decode().strip()


def clean(repo: Path) -> bool:
    return not git(
        repo, "status", "--porcelain=v1", "-z", "--untracked-files=all"
    )


def tree_sha256(repo: Path, revision: str) -> str:
    tree = git(repo, "ls-tree", "-r", "-z", "--full-tree", revision)
    return hashlib.sha256(tree).hexdigest()


def freeze(repo: Path, revision: str, destination: Path, expected: str) -> None:
    source_head = head(repo)
    source_hash = tree_sha256(repo, source_head)
    if not clean(repo):
        raise RuntimeError("source worktree is not clean")
    if source_head != revision:
        raise RuntimeError("source HEAD does not equal the capsule revision")
    if source_hash != expected:
        raise RuntimeError("source tracked-tree hash does not equal the capsule hash")
    if destination.exists():
        raise RuntimeError("immutable destination already exists")
    destination.parent.mkdir(parents=True, exist_ok=True)
    try:
        git(repo, "worktree", "add", "--detach", str(destination), revision)
        if head(destination) != revision or not clean(destination):
            raise RuntimeError("detached worktree does not reproduce the clean revision")
        if tree_sha256(destination, revision) != expected:
            raise RuntimeError("detached worktree hash differs from the source")
        if head(repo) != source_head or tree_sha256(repo, source_head) != source_hash or not clean(repo):
            raise RuntimeError("source worktree changed while freezing")
    except Exception:
        subprocess.run(
            ["git", "-C", str(repo), "worktree", "remove", "--force", str(destination)],
            check=False,
            capture_output=True,
        )
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    hash_parser = subparsers.add_parser("hash-tree")
    hash_parser.add_argument("repo", type=Path)
    hash_parser.add_argument("revision", nargs="?", default="HEAD")
    freeze_parser = subparsers.add_parser("freeze-project")
    freeze_parser.add_argument("repo", type=Path)
    freeze_parser.add_argument("revision")
    freeze_parser.add_argument("destination", type=Path)
    freeze_parser.add_argument("expected_sha256")
    args = parser.parse_args()
    try:
        if args.command == "hash-tree":
            print(tree_sha256(args.repo, args.revision))
        else:
            freeze(args.repo, args.revision, args.destination, args.expected_sha256)
            print(args.destination)
    except (OSError, RuntimeError, UnicodeError) as error:
        print(f"capsule: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
