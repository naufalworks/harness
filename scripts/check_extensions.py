#!/usr/bin/env python3
"""P18-T02 extension-contract checks. Offline, fail-closed, no network.

Trust model, decided by the owner on 2026-09-16: extensions are admitted by
*digest pin*, not by signature. There are currently no third-party extensions,
so a signature would only prove "signed by the one key this repo holds", which
is a ceremony rather than a control. Pinning gives the property that actually
matters today -- the bytes admitted are exactly the bytes reviewed -- and the
manifest carries a `schema` field so a `harness.extension/v2` can add a
`signature` block without reinterpreting any v1 manifest.

What runs where, stated plainly so neither side is mistaken for the other:

  * This script checks the *checked-in* extension corpus: pins resolve, digests
    match, payload files exist, no pin is orphaned, and no manifest declares
    network access or a capability outside the allowlist. It also asserts that
    the allowlist and schema string here match the Rust source, so the two
    implementations cannot drift apart silently.
  * Whether a manifest may call a given host *tool* is NOT checked here. That
    requires the real `tools::Registry`, so it is asserted in the Rust tests in
    src/plugins/mod.rs against `Registry::standard()`. A Python copy of that
    tool list would be a second source of truth that rots.

Exit code is 1 if any check fails, 0 otherwise.
"""

from __future__ import annotations

import hashlib
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PACKS = ROOT / "benchmarks" / "packs"
PINS = ROOT / "benchmarks" / "pinned.json"
PLUGINS_RS = ROOT / "src" / "plugins" / "mod.rs"

PIN_SCHEMA = "harness.extension-pins/v1"
HEX64 = re.compile(r"^[0-9a-f]{64}$")

failures: list[str] = []


def fail(area: str, message: str) -> None:
    failures.append(f"[FAIL] {area}: {message}")


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def payload_digest(files: list[dict]) -> str:
    """Digest over the payload *set*, not the manifest.

    A review cannot attest the manifest digest that contains the review, so the
    attestation covers the sorted `path sha256` lines instead. Reordering the
    files, swapping one file's digest, or adding a file all change this value.
    """
    lines = sorted(f"{entry['path']} {entry['sha256']}\n" for entry in files)
    return hashlib.sha256("".join(lines).encode("utf-8")).hexdigest()


def rust_string_const(text: str, name: str) -> str | None:
    match = re.search(rf'pub const {name}: &str = "([^"]+)"', text)
    return match.group(1) if match else None


def rust_capability_allowlist(text: str) -> list[str]:
    match = re.search(
        r"pub const ALLOWED_CAPABILITIES: &\[&str\] = &\[(.*?)\];", text, re.DOTALL
    )
    if not match:
        return []
    return sorted(re.findall(r'"([^"]+)"', match.group(1)))


# --------------------------------------------------------------------------
# 1. The Rust and Python views of the contract must agree.
# --------------------------------------------------------------------------
if not PLUGINS_RS.is_file():
    fail("contract", f"{PLUGINS_RS.relative_to(ROOT)} is missing")
    allowed_capabilities: list[str] = []
    manifest_schema = ""
else:
    rust = PLUGINS_RS.read_text(encoding="utf-8")
    allowed_capabilities = rust_capability_allowlist(rust)
    manifest_schema = rust_string_const(rust, "MANIFEST_SCHEMA") or ""
    if not allowed_capabilities:
        fail("contract", "could not read ALLOWED_CAPABILITIES from src/plugins/mod.rs")
    if not manifest_schema:
        fail("contract", "could not read MANIFEST_SCHEMA from src/plugins/mod.rs")

# --------------------------------------------------------------------------
# 2. Pin ledger shape.
# --------------------------------------------------------------------------
pins: dict[str, dict] = {}
if not PINS.is_file():
    fail("pins", f"{PINS.relative_to(ROOT)} is missing; extensions are admitted only by pin")
else:
    try:
        ledger = json.loads(PINS.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        ledger = {}
        fail("pins", f"{PINS.relative_to(ROOT)} is not valid JSON: {exc}")
    if ledger.get("schema") != PIN_SCHEMA:
        fail("pins", f"pin ledger schema must be {PIN_SCHEMA!r}, found {ledger.get('schema')!r}")
    pins = ledger.get("pins") or {}
    if not isinstance(pins, dict):
        fail("pins", "'pins' must be an object keyed by extension id")
        pins = {}
    for ext_id, pin in pins.items():
        if not isinstance(pin, dict) or not HEX64.match(str(pin.get("manifest_sha256", ""))):
            fail("pins", f"{ext_id}: manifest_sha256 must be a 64-character lowercase hex digest")

# --------------------------------------------------------------------------
# 3. Every checked-in manifest is pinned, matches its pin, and is bounded.
# --------------------------------------------------------------------------
manifest_paths = sorted(PACKS.glob("*/manifest.json")) if PACKS.is_dir() else []
seen_ids: set[str] = set()

for manifest_path in manifest_paths:
    rel = manifest_path.relative_to(ROOT)
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        fail("manifest", f"{rel} is not valid JSON: {exc}")
        continue

    ext_id = manifest.get("id", "")
    if not ext_id:
        fail("manifest", f"{rel} has no id")
        continue
    if ext_id in seen_ids:
        fail("manifest", f"{ext_id}: duplicate extension id")
    seen_ids.add(ext_id)

    if manifest_schema and manifest.get("schema") != manifest_schema:
        fail("manifest", f"{ext_id}: schema must be {manifest_schema!r}, found {manifest.get('schema')!r}")

    # Pin resolution and digest equality: the admitted bytes are the reviewed bytes.
    pin = pins.get(ext_id)
    if pin is None:
        fail("pins", f"{ext_id}: manifest is not pinned in benchmarks/pinned.json")
    else:
        actual = sha256_file(manifest_path)
        if actual != pin.get("manifest_sha256"):
            fail(
                "pins",
                f"{ext_id}: manifest digest {actual} does not match pinned "
                f"{pin.get('manifest_sha256')}; re-review before repinning",
            )
        if pin.get("kind") != manifest.get("kind"):
            fail("pins", f"{ext_id}: pinned kind {pin.get('kind')!r} disagrees with manifest kind {manifest.get('kind')!r}")

    # Bounded capabilities.
    for capability in manifest.get("capabilities") or []:
        if capability not in allowed_capabilities:
            fail("capability", f"{ext_id}: capability {capability!r} is outside the allowlist")
    if not manifest.get("capabilities"):
        fail("capability", f"{ext_id}: declares no capability; an extension with no bound is not admissible")

    # No extension gets network access in this phase.
    if manifest.get("network"):
        fail("network", f"{ext_id}: network access is not granted to extensions")

    # Payload files must exist, be inside the repo, and match their digests.
    files = manifest.get("files") or []
    for entry in files:
        path_value = str(entry.get("path", ""))
        if not path_value or path_value.startswith("/") or ".." in Path(path_value).parts:
            fail("payload", f"{ext_id}: payload path {path_value!r} must be repo-relative and must not escape the repo")
            continue
        target = ROOT / path_value
        if not target.is_file():
            fail("payload", f"{ext_id}: payload {path_value} does not exist")
            continue
        declared = str(entry.get("sha256", ""))
        if not HEX64.match(declared):
            fail("payload", f"{ext_id}: payload {path_value} has no valid sha256")
            continue
        actual = sha256_file(target)
        if actual != declared:
            fail("payload", f"{ext_id}: payload {path_value} digest {actual} does not match declared {declared}")

    # Audience review must attest the exact payload set.
    if manifest.get("kind") == "benchmark_pack":
        review = manifest.get("review")
        if not isinstance(review, dict):
            fail("review", f"{ext_id}: a benchmark pack ships shareable evidence and requires a review block")
        else:
            expected = payload_digest(files)
            if review.get("payload_sha256") != expected:
                fail(
                    "review",
                    f"{ext_id}: review attests payload {review.get('payload_sha256')} but the manifest "
                    f"declares {expected}; the review does not cover these files",
                )
            if not review.get("audience") or not review.get("reviewed_by"):
                fail("review", f"{ext_id}: review must name an audience and a reviewer")

# --------------------------------------------------------------------------
# 4. No orphan pins. A pin with no manifest is a permission left lying around.
# --------------------------------------------------------------------------
for ext_id in pins:
    if ext_id not in seen_ids:
        fail("pins", f"{ext_id}: pinned but no manifest ships it; remove the stale pin")

for line in failures:
    print(line, file=sys.stderr)

if failures:
    print(f"{len(failures)} failing extension check(s)", file=sys.stderr)
    sys.exit(1)

print(
    f"[PASS] extensions: {len(manifest_paths)} manifest(s), {len(pins)} pin(s); "
    "digests match, capabilities bounded, no network grant, reviews cover their payloads"
)
print("[INFO] host tool admission is asserted in Rust against the real registry, not here")
