#!/usr/bin/env python3
"""Validates tools/schemas/*.json: OpenAI function format, closed objects, names match files."""
import json
import pathlib
import re
import sys
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
SCHEMAS = ROOT / "tools" / "schemas"
EXPECTED = {"read", "grep", "glob", "edit", "write", "bash", "think", "todo_write", "skill", "task", "ast_edit"}
NAME_RE = re.compile(r"^[a-z][a-z0-9_]{0,63}$")


def walk_objects(node, path):
    """Every object schema must be closed and every required key must exist."""
    if isinstance(node, dict):
        if node.get("type") == "object":
            assert node.get("additionalProperties") is False, f"{path}: additionalProperties must be false"
            props = node.get("properties", {})
            for r in node.get("required", []):
                assert r in props, f"{path}: required '{r}' not in properties"
            for k, v in props.items():
                assert "description" in v or "enum" in v or v.get("type") == "object" or v.get("type") == "array", f"{path}.{k}: needs a description"
        for k, v in node.items():
            walk_objects(v, f"{path}.{k}")
    elif isinstance(node, list):
        for i, v in enumerate(node):
            walk_objects(v, f"{path}[{i}]")


def main():
    files = sorted(SCHEMAS.glob("*.json"))
    names = set()
    for f in files:
        doc = json.loads(f.read_text())
        assert doc.get("type") == "function", f"{f.name}: type must be 'function'"
        fn = doc["function"]
        name = fn["name"]
        assert NAME_RE.match(name), f"{f.name}: bad tool name {name!r}"
        assert name == f.stem, f"{f.name}: name {name!r} does not match file"
        assert 20 <= len(fn["description"]) <= 1024, f"{f.name}: description length"
        assert fn["parameters"]["type"] == "object", f"{f.name}: parameters must be an object"
        walk_objects(fn["parameters"], name)
        assert len(json.dumps(doc)) < 6000, f"{f.name}: schema too large for the context budget"
        names.add(name)
    missing = EXPECTED - names
    extra = names - EXPECTED
    assert not missing, f"missing schemas: {missing}"
    assert not extra, f"unexpected schemas (add to EXPECTED and docs/design/tools.md): {extra}"
    total = sum(len(f.read_text()) for f in files)
    print(f"tool schemas OK: {len(files)} files, {total} bytes total")


class Suite(unittest.TestCase):
    """Lets `python3 -m unittest discover -s tests` pick this file up."""

    def test_all(self):
        main()


if __name__ == "__main__":
    try:
        main()
    except (AssertionError, KeyError) as e:
        print("FAIL:", e, file=sys.stderr)
        sys.exit(1)
