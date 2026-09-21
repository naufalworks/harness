#!/usr/bin/env python3
"""Offline contract check for harness.external-history/v1. No Rust toolchain needed.

Validates the fixture corpus in tests/external_history against the envelope rules in
docs/design/external-development-history.md. Enforcement lands in P19-T02/T03; this
suite pins the contract so a malformed event is rejected before any ingestion handler
exists.
"""
import copy
import datetime
import hashlib
import json
import pathlib
import re
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests" / "external_history"
DESIGN = ROOT / "docs" / "design" / "external-development-history.md"

SCHEMA = "harness.external-history/v1"
REQUIRED = ("schema_version", "event_id", "content_digest", "producer_id",
            "producer_instance_id", "producer_sequence", "project_id",
            "logical_session_id", "occurred_at", "event_type", "outcome",
            "capture", "payload")
EVENT_TYPES = {"session.opened", "session.closed", "tool.admitted", "tool.started",
               "tool.completed", "tool.failed", "request.rejected", "task.started",
               "task.output", "task.completed", "task.interrupted",
               "artifact.recorded", "capture.gap", "message.observed"}
TRANSPORT = {"ok", "error"}
EXECUTION = {"succeeded", "failed", "unknown", "not_applicable"}
CONVERSATION = {"client_supplied", "unavailable", "not_supported"}
PAYLOAD_CAPTURE = {"full", "summarized", "omitted"}
RFC3339 = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:\d{2})$")


def digest(event):
    """Canonical UTF-8 JSON: sorted ASCII keys, compact, integers only, no digest."""
    body = {k: v for k, v in event.items() if k != "content_digest"}
    return hashlib.sha256(json.dumps(body, sort_keys=True, separators=(",", ":"),
                                    ensure_ascii=False, allow_nan=False).encode()).hexdigest()


def validate(event, seen=None):
    """Offline structural conformance only; NOT auth, redaction or durable storage."""
    if not isinstance(event, dict):
        return "malformed_envelope"
    for field in REQUIRED:
        if event.get(field) is None:
            return "missing_required_field"
    if event["schema_version"] != SCHEMA:
        return "unsupported_schema_version"
    if set(event) - set(REQUIRED) - {"invocation_id", "task_id"}:
        return "malformed_envelope"
    def bounded(value, depth=0):
        if depth > 12:
            return False
        if value is None or type(value) is bool:
            return True
        if type(value) is int:
            return abs(value) <= 9007199254740991
        if isinstance(value, str):
            try:
                return len(value.encode()) <= 16384
            except UnicodeError:
                return False
        if isinstance(value, list):
            return len(value) <= 256 and all(bounded(x, depth+1) for x in value)
        if isinstance(value, dict):
            return len(value) <= 256 and all(isinstance(k, str) and re.fullmatch(r"[a-zA-Z_][a-zA-Z0-9_]{0,63}", k) and bounded(v, depth+1) for k,v in value.items())
        return False
    if not bounded(event):
        return "malformed_envelope"
    if len(json.dumps(event, ensure_ascii=False).encode()) > 65536:
        return "malformed_envelope"
    for key in ("event_id", "producer_id", "producer_instance_id", "project_id", "logical_session_id"):
        if not isinstance(event[key], str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}", event[key]):
            return "malformed_envelope"
    if type(event["producer_sequence"]) is not int or event["producer_sequence"] < 1:
        return "malformed_envelope"
    etype = event["event_type"]
    if not isinstance(etype, str) or etype not in EVENT_TYPES:
        return "unknown_event_type"
    for prefix, key in (("tool.", "invocation_id"), ("task.", "task_id")):
        if etype.startswith(prefix) and not event.get(key):
            return "missing_required_field"
        if event.get(key) is not None and (not isinstance(event[key], str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.:-]{0,127}",event[key])):
            return "malformed_envelope"
    stamp = event["occurred_at"]
    try:
        if not isinstance(stamp,str) or not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\.[0-9]{1,6})?Z",stamp):
            raise ValueError()
        datetime.datetime.fromisoformat(stamp.replace("Z", "+00:00"))
    except ValueError:
        return "invalid_timestamp"
    outcome, capture, payload = event["outcome"], event["capture"], event["payload"]
    if not all(isinstance(x,dict) for x in (outcome,capture,payload)):
        return "malformed_envelope"
    if set(outcome) != {"transport","execution","exit_code"} or set(capture) != {"conversation","payload","truncated"}:
        return "malformed_envelope"
    if outcome["transport"] not in ("ok","error") or outcome["execution"] not in tuple(EXECUTION):
        return "malformed_envelope"
    code=outcome["exit_code"]
    if code is not None and (type(code) is not int or not -2147483648 <= code <= 2147483647):
        return "malformed_envelope"
    if code not in (None,0) and outcome["execution"] == "succeeded":
        return "malformed_envelope"
    if etype == "task.interrupted" and outcome["execution"] != "unknown":
        return "malformed_envelope"
    if capture["conversation"] not in tuple(CONVERSATION) or capture["payload"] not in tuple(PAYLOAD_CAPTURE) or type(capture["truncated"]) is not bool:
        return "malformed_envelope"
    if etype == "message.observed" and (capture["conversation"] != "client_supplied" or payload.get("role") not in ("user","assistant") or not isinstance(payload.get("text"),str) or not payload.get("source_client")):
        return "malformed_envelope"
    if etype == "capture.gap" and not (type(payload.get("missing_from_sequence")) is int and type(payload.get("missing_to_sequence")) is int and 1 <= payload["missing_from_sequence"] <= payload["missing_to_sequence"]):
        return "malformed_envelope"
    if etype == "artifact.recorded":
        if (not isinstance(payload.get("artifact_id"), str)
                or not payload["artifact_id"]
                or not isinstance(payload.get("media_type"), str)
                or type(payload.get("byte_count")) is not int
                or payload["byte_count"] < 0
                or not isinstance(payload.get("digest"), str)
                or not re.fullmatch(r"[0-9a-f]{64}", payload["digest"])
                or type(payload.get("truncated")) is not bool):
            return "malformed_envelope"
    if not isinstance(event["content_digest"],str) or not re.fullmatch(r"[0-9a-f]{64}",event["content_digest"]):
        return "invalid_digest"
    if digest(event) != event["content_digest"]:
        return "invalid_digest"
    if seen is not None:
        prior=seen.get((event["producer_id"],event["event_id"]))
        if prior is not None and prior != event["content_digest"]:
            return "duplicate_event_id_conflict"
    return None


def load(sub):
    return sorted((FIXTURES / sub).glob("*.json"))


class TestAccepted(unittest.TestCase):
    def test_corpus_present(self):
        self.assertGreaterEqual(len(load("accepted")), 7)
        self.assertGreaterEqual(len(load("rejected")), 10)

    def test_accepted_fixtures_validate(self):
        for path in load("accepted"):
            with self.subTest(fixture=path.name):
                self.assertIsNone(validate(json.loads(path.read_text())))

    def test_transport_ok_execution_failed_is_accepted(self):
        """A handler returning normally must not imply the command succeeded."""
        e = json.loads((FIXTURES / "accepted" / "transport_ok_execution_failed.json").read_text())
        self.assertIsNone(validate(e))
        self.assertEqual(e["outcome"]["transport"], "ok")
        self.assertEqual(e["outcome"]["execution"], "failed")
        self.assertEqual(e["outcome"]["exit_code"], 1)

    def test_crash_window_stays_unknown(self):
        e = json.loads((FIXTURES / "accepted" / "crash_window_unknown.json").read_text())
        self.assertIsNone(validate(e))
        self.assertEqual(e["outcome"]["execution"], "unknown")

    def test_missing_conversation_is_explicit_not_empty(self):
        e = json.loads((FIXTURES / "accepted" / "conversation_unavailable.json").read_text())
        self.assertIsNone(validate(e))
        self.assertEqual(e["capture"]["conversation"], "unavailable")
        self.assertNotIn("messages", e["payload"])

    def test_idempotent_replay_same_digest(self):
        first = json.loads((FIXTURES / "accepted" / "tool_completed.json").read_text())
        replay = json.loads((FIXTURES / "accepted" / "idempotent_replay.json").read_text())
        self.assertEqual(first["event_id"], replay["event_id"])
        self.assertEqual(first["content_digest"], replay["content_digest"])
        seen = {(first["producer_id"], first["event_id"]): first["content_digest"]}
        self.assertIsNone(validate(replay, seen))

    def test_late_arrival_accepted(self):
        e = json.loads((FIXTURES / "accepted" / "late_arrival.json").read_text())
        self.assertIsNone(validate(e))


class TestRejected(unittest.TestCase):
    def test_each_rejection_returns_declared_code(self):
        first = json.loads((FIXTURES / "accepted" / "tool_completed.json").read_text())
        seen = {(first["producer_id"], first["event_id"]): first["content_digest"]}
        for path in load("rejected"):
            with self.subTest(fixture=path.name):
                event = json.loads(path.read_text())
                expected = event.pop("__expect")
                self.assertEqual(validate(event, seen), expected)

    def test_rejection_codes_documented(self):
        text = DESIGN.read_text()
        for path in load("rejected"):
            code = json.loads(path.read_text())["__expect"]
            with self.subTest(code=code):
                self.assertIn(code, text)


class TestDesignDoc(unittest.TestCase):
    def test_design_document_exists_and_pins_schema(self):
        self.assertTrue(DESIGN.is_file())
        text = DESIGN.read_text()
        self.assertIn(SCHEMA, text)
        for name in EVENT_TYPES:
            self.assertIn(name, text)

    def test_no_agent_loop_entry_claimed(self):
        text = DESIGN.read_text().lower()
        self.assertIn("does not submit chat turns, invoke tools, or call a provider", text)


class TestMalformedInputs(unittest.TestCase):
    def base(self):
        return json.loads((FIXTURES / "accepted" / "tool_completed.json").read_text())

    def test_non_object_and_wrong_field_types(self):
        for value in (None, [], "text", 1, True):
            self.assertEqual(validate(value), "malformed_envelope")
        for key, value in (("producer_sequence", True), ("producer_sequence", 0),
                           ("event_id", ""), ("outcome", []), ("capture", []),
                           ("event_type", []), ("payload", [])):
            with self.subTest(field=key):
                event = self.base()
                event[key] = value
                self.assertIsNotNone(validate(event))

    def test_real_calendar_validation(self):
        event = self.base()
        event["occurred_at"] = "2026-02-30T00:00:00Z"
        self.assertEqual(validate(event), "invalid_timestamp")

    def test_digest_detects_changed_evidence(self):
        event = self.base()
        event["payload"]["path"] = "other.rs"
        self.assertEqual(validate(event), "invalid_digest")

    def test_producer_scoped_identity(self):
        event = self.base()
        self.assertIsNone(validate(event, {("another-producer", event["event_id"]): "0" * 64}))

    def test_message_requires_client_provenance(self):
        event = json.loads((FIXTURES / "accepted" / "message_observed_client_supplied.json").read_text())
        event["capture"]["conversation"] = "unavailable"
        self.assertEqual(validate(event), "malformed_envelope")

    def test_artifact_metadata_is_required(self):
        event = json.loads((FIXTURES / "accepted" / "artifact.recorded.json").read_text())
        event["payload"]["byte_count"] = -1
        self.assertEqual(validate(event), "malformed_envelope")

    def test_all_event_types_have_accepted_examples(self):
        types = {json.loads(p.read_text())["event_type"] for p in load("accepted")}
        self.assertEqual(types, EVENT_TYPES)


if __name__ == "__main__":
    unittest.main(verbosity=2)
