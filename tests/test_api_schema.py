#!/usr/bin/env python3
"""Check that docs/api.yaml describes the API the code actually serves.

P12-T03 asks for a checked OpenAPI contract. A contract nobody compares against
the implementation is worse than none, because it is quoted as if it were true.
So this test is the comparison, in both directions, against three independent
sources that must agree:

  1. `router()` in src/api/routes.rs      - the routes the process really serves
  2. the HTTP surface inventory in
     docs/ARCHITECTURE.md                 - the human-maintained list the docs
                                           gate already enforces
  3. docs/api.yaml                        - this contract

The inventory records methods (`GET|POST /config`), so it is a genuine second
opinion on the method set, not just the paths that scripts/check_docs.py
already compares.

It also compares the error-code enum in the contract against the `error_code`
table in src/api/error.rs, so a new code cannot be added to the server without
appearing in the published contract.

Run: python3 tests/test_api_schema.py
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
ROUTES_RS = ROOT / "src" / "api" / "routes.rs"
ERROR_RS = ROOT / "src" / "api" / "error.rs"
ARCHITECTURE = ROOT / "docs" / "ARCHITECTURE.md"
CONTRACT = ROOT / "docs" / "api.yaml"

HTTP_METHODS = ("get", "post", "put", "patch", "delete")

# Routes that do not answer with the JSON error envelope. The asset routes serve
# HTML/JS/CSS and are reached before any JSON handler, so requiring an ApiError
# response on them would document a body they never produce.
NON_JSON_OPERATIONS = {("get", "/"), ("get", "/app.js"), ("get", "/style.css")}

failures: list[str] = []


def fail(message: str) -> None:
    failures.append(message)


def router_operations() -> set[tuple[str, str]]:
    """Method+path pairs served by `router()`.

    Splitting on `.route(` and reading the method calls inside each chunk is what
    makes the multi-method routes visible: `get(get_config).post(set_config)` is
    two operations on one path, and a path-only comparison would miss the second.
    """
    source = ROUTES_RS.read_text(encoding="utf-8")
    start = source.find("pub(crate) fn router")
    if start < 0:
        fail(f"no router function found in {ROUTES_RS}")
        return set()
    operations: set[tuple[str, str]] = set()
    for chunk in source[start:].split(".route(")[1:]:
        path_match = re.search(r'"([^"]+)"', chunk)
        if not path_match:
            continue
        path = path_match.group(1)
        # Stop at the next route so a chunk cannot claim the following one's
        # methods; the split already bounds it, but the layer calls trailing the
        # last route are cut here too.
        body = chunk.split(".route_layer(")[0].split("Router::new()")[0]
        for method in HTTP_METHODS:
            if re.search(rf"\b{method}\(", body):
                operations.add((method, path))
    return operations


def inventory_operations() -> set[tuple[str, str]]:
    """Method+path pairs listed in the docs/ARCHITECTURE.md HTTP surface."""
    text = ARCHITECTURE.read_text(encoding="utf-8")
    section = re.search(r"^## HTTP surface$(.*?)(?=^## |\Z)", text, re.DOTALL | re.MULTILINE)
    if not section:
        fail("docs/ARCHITECTURE.md has no '## HTTP surface' inventory section")
        return set()
    operations: set[tuple[str, str]] = set()
    for methods, path in re.findall(r"`((?:GET|POST|PUT|DELETE|PATCH)(?:\|(?:GET|POST|PUT|DELETE|PATCH))*) ([^`]+)`", section.group(1)):
        for method in methods.split("|"):
            operations.add((method.lower(), path))
    return operations


def contract_operations(spec: dict) -> set[tuple[str, str]]:
    operations: set[tuple[str, str]] = set()
    for path, item in (spec.get("paths") or {}).items():
        for method in item:
            if method in HTTP_METHODS:
                operations.add((method, path))
    return operations


def server_error_codes() -> set[str]:
    """The codes `error_code` in src/api/error.rs can return."""
    source = ERROR_RS.read_text(encoding="utf-8")
    start = source.find("pub(crate) fn error_code")
    if start < 0:
        fail(f"no error_code function found in {ERROR_RS}")
        return set()
    body = source[start : source.find("\n}", start)]
    return set(re.findall(r'=>\s*"([a-z_]+)"', body))


def compare(label_a: str, a: set, label_b: str, b: set) -> None:
    for item in sorted(a - b):
        fail(f"{label_a} has {item[0].upper()} {item[1]}, missing from {label_b}")
    for item in sorted(b - a):
        fail(f"{label_b} has {item[0].upper()} {item[1]}, missing from {label_a}")


def main() -> int:
    if not CONTRACT.exists():
        print(f"[FAIL] api-schema: {CONTRACT} is missing")
        return 1
    spec = yaml.safe_load(CONTRACT.read_text(encoding="utf-8"))

    # Structural sanity: without these the rest of the comparisons are vacuous.
    if not isinstance(spec, dict) or not spec.get("openapi", "").startswith("3."):
        fail("docs/api.yaml is not an OpenAPI 3.x document")
    if not (spec.get("paths") or {}):
        fail("docs/api.yaml declares no paths")

    router = router_operations()
    contract = contract_operations(spec)
    inventory = inventory_operations()

    compare("the router", router, "docs/api.yaml", contract)
    compare("the router", router, "the ARCHITECTURE.md inventory", inventory)

    # Error envelope: shape, and every JSON operation referencing it.
    schemas = ((spec.get("components") or {}).get("schemas") or {})
    envelope = schemas.get("ApiError")
    if not envelope:
        fail("docs/api.yaml has no components.schemas.ApiError envelope")
    else:
        required = set(envelope.get("required") or [])
        if required != {"error", "code", "retryable"}:
            fail(f"ApiError must require error/code/retryable, found {sorted(required)}")
        documented = set((envelope.get("properties") or {}).get("code", {}).get("enum") or [])
        actual = server_error_codes()
        for code in sorted(actual - documented):
            fail(f"src/api/error.rs can return code {code!r}, absent from docs/api.yaml")
        for code in sorted(documented - actual):
            fail(f"docs/api.yaml documents code {code!r}, which src/api/error.rs never returns")

    for method, path in sorted(contract):
        if (method, path) in NON_JSON_OPERATIONS:
            continue
        responses = ((spec["paths"][path][method]).get("responses") or {})
        text = yaml.safe_dump(responses)
        if "ApiError" not in text and "responses/Error" not in text:
            fail(f"{method.upper()} {path} documents no error response using the shared envelope")

    if failures:
        for message in failures:
            print(f"[FAIL] api-schema: {message}")
        print(f"\n{len(failures)} failing check(s)")
        return 1
    print(
        f"[PASS] api-schema: {len(contract)} documented operations match the router "
        f"and the HTTP surface inventory exactly"
    )
    print(f"[PASS] api-schema: error envelope and {len(server_error_codes())} error codes match src/api/error.rs")
    return 0


if __name__ == "__main__":
    sys.exit(main())
