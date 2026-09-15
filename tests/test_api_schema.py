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

Finally it compares the frontend client in static/api.js against the same two
sources. The client is the fourth place that has to agree: it mirrors the error
codes it can branch on and the request states it can label, and a mirror nobody
checks is just a stale copy waiting to mislabel a state or silently stop
matching a renamed code.

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
RECORDING_RS = ROOT / "src" / "recording.rs"
ARCHITECTURE = ROOT / "docs" / "ARCHITECTURE.md"
CONTRACT = ROOT / "docs" / "api.yaml"
API_JS = ROOT / "static" / "api.js"

HTTP_METHODS = ("get", "post", "put", "patch", "delete")

# Routes that do not answer with the JSON error envelope. The asset routes serve
# HTML/JS/CSS and are reached before any JSON handler, so requiring an ApiError
# response on them would document a body they never produce.
NON_JSON_OPERATIONS = {
    ("get", "/"),
    ("get", "/api.js"),
    ("get", "/app.js"),
    ("get", "/style.css"),
}

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


def server_request_states() -> set[str]:
    """The durable strings `recording::RequestState` can represent.

    The enum serialises with `rename_all = "lowercase"`, so the variant names
    lowercased are exactly the strings that reach the wire.
    """
    source = RECORDING_RS.read_text(encoding="utf-8")
    start = source.find("pub enum RequestState {")
    if start < 0:
        fail(f"no RequestState enum found in {RECORDING_RS}")
        return set()
    body = source[start : source.find("}", start)]
    return {name.lower() for name in re.findall(r"^\s{4}([A-Z][A-Za-z]*),", body, re.MULTILINE)}


def server_receipt_fields() -> set[str]:
    """The keys the receipt builder in src/recording.rs actually emits."""
    source = RECORDING_RS.read_text(encoding="utf-8")
    start = source.find("fn receipt(")
    if start < 0:
        fail(f"no receipt builder found in {RECORDING_RS}")
        return set()
    body = source[start : source.find("\n}", start)]
    return set(re.findall(r'"([a-z_]+)"\s*:', body))


def client_frozen_list(name: str) -> set[str]:
    """The string members of a `const <name> = Object.freeze([...])` in api.js.

    Reading the source rather than executing it keeps this test dependency-free:
    there is no Node requirement and nothing from the client runs here.
    """
    source = API_JS.read_text(encoding="utf-8")
    match = re.search(rf"const {name} = Object\.freeze\(\[(.*?)\]\)", source, re.DOTALL)
    if not match:
        fail(f"no {name} list found in {API_JS}")
        return set()
    return set(re.findall(r"'([a-z_]+)'", match.group(1)))


def client_label_keys() -> set[str]:
    """The states static/api.js has a reader-facing label for."""
    source = API_JS.read_text(encoding="utf-8")
    match = re.search(r"const REQUEST_STATE_LABELS = Object\.freeze\(\{(.*?)\}\)", source, re.DOTALL)
    if not match:
        fail(f"no REQUEST_STATE_LABELS map found in {API_JS}")
        return set()
    return set(re.findall(r"^\s*([a-z_]+):", match.group(1), re.MULTILINE))


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

    # The receipt is the one success payload this contract specifies, so it is
    # held to the same standard as the error envelope: compared field for field
    # against the code that builds it, in both directions.
    states = schemas.get("RequestState")
    if not states:
        fail("docs/api.yaml has no components.schemas.RequestState")
    else:
        documented = set(states.get("enum") or [])
        actual = server_request_states()
        for state in sorted(actual - documented):
            fail(f"recording::RequestState defines {state!r}, absent from docs/api.yaml")
        for state in sorted(documented - actual):
            fail(f"docs/api.yaml documents state {state!r}, which recording::RequestState does not define")

    receipt = schemas.get("Receipt")
    if not receipt:
        fail("docs/api.yaml has no components.schemas.Receipt")
    else:
        properties = set(receipt.get("properties") or {})
        emitted = server_receipt_fields()
        for field in sorted(emitted - properties):
            fail(f"the receipt builder emits {field!r}, absent from docs/api.yaml")
        for field in sorted(properties - emitted):
            fail(f"docs/api.yaml documents receipt field {field!r}, which the builder never emits")
        # The builder emits every key on every row, using null for absence, so
        # a client may read any field without probing for it. Requiring all of
        # them keeps that promise from quietly weakening.
        if set(receipt.get("required") or []) != properties:
            fail("Receipt must require every field it documents; the builder always emits all of them")

    # The frontend client mirrors the codes it branches on and the states it
    # labels. Both are compared against the server, so a rename on either side
    # fails here instead of degrading into a missed branch or a raw identifier
    # rendered at the reader.
    client_codes = client_frozen_list("API_ERROR_CODES")
    if client_codes:
        actual = server_error_codes()
        for code in sorted(actual - client_codes):
            fail(f"src/api/error.rs can return code {code!r}, unknown to static/api.js")
        for code in sorted(client_codes - actual):
            fail(f"static/api.js expects code {code!r}, which src/api/error.rs never returns")
        # Every code a branch depends on must be one the server can really send.
        for code in sorted(client_frozen_list("NOT_ADMITTED_CODES") - actual):
            fail(f"static/api.js treats {code!r} as a non-admission, but the server never sends it")

    client_states = client_frozen_list("REQUEST_STATES")
    if client_states:
        actual = server_request_states()
        for state in sorted(actual - client_states):
            fail(f"recording::RequestState defines {state!r}, unknown to static/api.js")
        for state in sorted(client_states - actual):
            fail(f"static/api.js expects state {state!r}, which recording::RequestState does not define")
        for state in sorted(client_states - client_label_keys()):
            fail(f"static/api.js has no reader-facing label for state {state!r}")
        for state in sorted(client_frozen_list("RETRYABLE_REQUEST_STATES") - actual):
            fail(f"static/api.js offers retry for state {state!r}, which is not a recorded state")

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
    print(
        f"[PASS] api-schema: receipt schema matches the builder "
        f"({len(server_receipt_fields())} fields, {len(server_request_states())} states) in src/recording.rs"
    )
    print(
        f"[PASS] api-schema: static/api.js mirrors {len(client_codes)} error codes "
        f"and {len(client_states)} labelled request states"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
