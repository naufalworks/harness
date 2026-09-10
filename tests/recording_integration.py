#!/usr/bin/env python3
"""RELEASE GATE: compiled Rust server + scripted OpenAI-style loopback provider.

No paid provider calls. This covers durable admission, the P1 agent loop, permissions,
sandbox failures, budgets, and restart recovery over the actual HTTP surface.
"""
from __future__ import annotations

import json
import os
import socket
import shutil
import sqlite3
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

from mock_provider import MockProvider, failure, text, tool_calls

ROOT = Path(__file__).resolve().parents[1]

def port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def main() -> None:
    binary = ROOT / "target/debug/harness"
    if not binary.is_file():
        raise SystemExit("NOT RUN: build first with cargo build --locked")

    beta_hash = __import__("hashlib").sha256(b"beta").hexdigest()[:4]
    stale_hash = __import__("hashlib").sha256(b"not-the-current-line").hexdigest()[:4]
    # P5-T03: a sub-agent's own context is `Exploration: <description>\n\n<prompt>`, so its calls
    # select their own scenario here and can never consume the parent's scripted replies.
    explore_ask = "find gamma"
    explore_prompt = "which line of notes.md holds gamma"
    explore_key = f"Exploration: {explore_ask}\n\n{explore_prompt}"
    scripts = {
        "I prefer Rust": [text("Synthetic durable answer")],
        "simulate provider failure": [failure(503, "synthetic provider failure")],
        "rename beta to gamma": [
            tool_calls(("read-1", "read", {"path": "notes.md", "offset": 1, "limit": 20})),
            tool_calls(("edit-1", "edit", {"path": "notes.md", "anchors": [{"line": 2, "hash": beta_hash}], "end_line": 2, "new_string": "gamma"})),
            tool_calls(("bash-1", "bash", {"command": "printf 'unit tests passed\\n'", "description": "run unit tests"})),
            text("Read notes.md, changed beta to gamma, and ran the unit tests."),
        ],
        "stale anchor": [
            tool_calls(("read-stale", "read", {"path": "stale.md"})),
            tool_calls(("edit-stale", "edit", {"path": "stale.md", "anchors": [{"line": 2, "hash": stale_hash}], "end_line": 2, "new_string": "changed"})),
            text("The edit was refused because the anchor was stale."),
        ],
        "path escape": [
            tool_calls(("escape-1", "read", {"path": "../../etc/passwd"})),
            text("The requested path was outside the project and was not read."),
        ],
        "delegate the search": [
            tool_calls(("task-1", "task", {"description": explore_ask, "prompt": explore_prompt})),
            text("A sub-agent found gamma on line 2."),
        ],
        explore_key: [
            tool_calls(
                ("sub-read", "read", {"path": "notes.md"}),
                ("sub-write", "write", {"path": "deny.md", "content": "should not land\n", "overwrite": True}),
            ),
            text("notes.md line 2 holds gamma."),
        ],
        "deny write": [
            tool_calls(("deny-1", "write", {"path": "deny.md", "content": "should not land\\n", "overwrite": True})),
            text("The write was denied, so I left the file unchanged."),
        ],
        "budget test": [
            tool_calls(("budget-1", "read", {"path": "notes.md"})),
        ],
        "stream a turn": [text("Streamed answer")],
        "hold during tool": [
            tool_calls(("sleep-1", "bash", {"command": "echo started; sleep 30", "description": "hold for restart"})),
            text("This answer must never be reached after the crash."),
        ],
    }
    provider = MockProvider(scripts)
    provider.start()

    project_tmp = Path(tempfile.mkdtemp(prefix="harness-project-"))
    try:
      with tempfile.TemporaryDirectory(prefix="recording-integration-") as tmp:
        tmp_path = Path(tmp)
        root = project_tmp
        (root / "notes.md").write_text("alpha\nbeta\n")
        (root / "stale.md").write_text("alpha\nbeta\n")
        (root / "deny.md").write_text("keep this file\n")
        # P5-T02: one real skill, so the receipt proves discovery is wired into a recorded turn.
        (root / "skills" / "review").mkdir(parents=True)
        (root / "skills" / "review" / "SKILL.md").write_text(
            "---\ndescription: How this repo reviews a diff\n---\nRe-anchor before every edit.\n"
        )
        db = tmp_path / "fixture.db"
        server_port = port()
        token = "synthetic-" + "x" * 40
        env = {
            **os.environ,
            "HARNESS_DB": str(db),
            "HARNESS_AUTH_TOKEN": token,
            "HARNESS_API_KEY": "synthetic",
            "HARNESS_BASE_URL": f"http://127.0.0.1:{provider.port}",
            "HARNESS_ADDR": f"127.0.0.1:{server_port}",
            "HARNESS_MODEL": "synthetic-model",
        }
        app: subprocess.Popen[bytes] | None = None

        def call(path: str, body: dict | None = None, auth: bool = True, origin: str | None = None):
            headers = {"Content-Type": "application/json"}
            if auth:
                headers["Authorization"] = "Bearer " + token
            if origin:
                headers["Origin"] = origin
            request = urllib.request.Request(
                f"http://127.0.0.1:{server_port}{path}",
                data=None if body is None else json.dumps(body).encode(),
                headers=headers,
                method="POST" if body is not None else "GET",
            )
            try:
                with urllib.request.urlopen(request, timeout=10) as response:
                    return response.status, json.load(response)
            except urllib.error.HTTPError as error:
                return error.code, json.loads(error.read())

        def open_stream(path: str, timeout: float = 8):
            """P2-T01: an SSE reader. Never pass a streaming path to `call`; it reads to EOF."""
            request = urllib.request.Request(f"http://127.0.0.1:{server_port}{path}", headers={"Authorization": "Bearer " + token})
            return urllib.request.urlopen(request, timeout=timeout)

        def read_frames(response, wanted: int, seconds: float = 10) -> list[tuple[int, str, dict]]:
            frames: list[tuple[int, str, dict]] = []
            deadline = time.time() + seconds
            seq: int | None = None
            kind: str | None = None
            while len(frames) < wanted and time.time() < deadline:
                try:
                    line = response.readline().decode()
                except OSError:  # socket timeout: an idle stream, which is a result too
                    break
                if not line:
                    break
                line = line.rstrip("\n")
                if line.startswith("id:"):
                    seq = int(line[3:].strip())
                elif line.startswith("event:"):
                    kind = line[6:].strip()
                elif line.startswith("data:"):
                    frames.append((seq, kind, json.loads(line[5:].strip())))
            return frames

        def start() -> subprocess.Popen[bytes]:
            process = subprocess.Popen([str(binary)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            for _ in range(100):
                if process.poll() is not None:
                    raise AssertionError("server exited during startup")
                try:
                    if call("/memory/status")[0] == 200:
                        return process
                except urllib.error.URLError:
                    pass
                time.sleep(0.1)
            process.kill()
            process.wait()
            raise AssertionError("server startup timeout")

        def submit(prompt: str, session: str | None = None) -> dict:
            body = {"request_id": str(uuid.uuid4()), "session_id": session or str(uuid.uuid4()), "scope": "global", "prompt": prompt}
            code, receipt = call("/chat/submit", body)
            assert code in (200, 202), (code, receipt)
            return body

        def wait_receipt(request_id: str, state: str, timeout: float = 15) -> dict:
            deadline = time.time() + timeout
            last: dict = {}
            while time.time() < deadline:
                code, last = call("/chat/requests/" + request_id)
                if code == 200 and last.get("state") == state:
                    return last
                time.sleep(0.05)
            raise AssertionError(("receipt timeout", state, last))

        def wait_db(request_id: str, predicate, timeout: float = 10) -> list[tuple]:
            deadline = time.time() + timeout
            last: list[tuple] = []
            while time.time() < deadline:
                with sqlite3.connect(db) as connection:
                    last = connection.execute("SELECT seq,kind,status,tool_name,error_code FROM turn_steps WHERE request_id=? ORDER BY seq", (request_id,)).fetchall()
                if predicate(last):
                    return last
                time.sleep(0.05)
            raise AssertionError(("step timeout", request_id, last))

        def configure(**patch: object) -> None:
            payload = {"root_path": str(root), **patch}
            code, body = call("/scopes/global", payload)
            assert code == 200, (code, body)

        try:
            app = start()
            configure(permission_mode="auto_all", max_steps=40, max_tool_bytes=400000, max_wall_seconds=900)

            # Existing durable recording contract, now with tools present in the first request.
            first = submit("I prefer Rust")
            done = wait_receipt(first["request_id"], "complete")
            assert done["response"] == "Synthetic durable answer"
            assert provider.count("I prefer Rust") == 1
            first_provider = next(item["body"] for item in provider.requests if item["scenario"] == "I prefer Rust")
            schema_count = len(list((ROOT / "tools/schemas").glob("*.json")))
            assert len(first_provider["tools"]) == schema_count and first_provider["tool_choice"] == "auto"
            assert call("/chat/submit", first)[0] == 200
            assert provider.count("I prefer Rust") == 1, "idempotent replay must not call the provider"
            changed = {**first, "prompt": "Changed content"}
            assert call("/chat/submit", changed)[0] == 409
            detail = call("/chat/requests/" + first["request_id"] + "/context")[1]
            assert detail["context"]["provider_messages"] == first_provider["messages"]
            assert detail["context"]["provider_tools"] == first_provider["tools"]
            assert detail["context"]["model"] == first_provider["model"]
            assert detail["context"]["format_version"] == 2
            budget = detail["context"]["context_receipt"]
            categories = budget["categories"]
            assert [row["name"] for row in categories] == [
                "system_rules", "tool_definitions", "skills_index", "repo_map",
                "recalled_memories", "plan", "compacted_history", "recent_steps", "user_message",
            ]
            assert all(row["candidate_bytes"] == row["included_bytes"] + row["excluded_bytes"] for row in categories)
            assert all(row["included_bytes"] <= row["budget_bytes"] for row in categories)
            assert budget["totals"]["candidate_bytes"] == budget["totals"]["included_bytes"] + budget["totals"]["excluded_bytes"]
            skills_row = next(row for row in categories if row["name"] == "skills_index")
            assert [part["id"] for part in skills_row["included_parts"]] == ["skill:review"], skills_row
            window = "\n".join(str(message.get("content", "")) for message in first_provider["messages"])
            assert "review: How this repo reviews a diff" in window, "the skills index reaches the first window"
            assert "Re-anchor before every edit" not in window, "a skill body must never be in the initial window"
            assert call("/chat/requests/" + first["request_id"], auth=False)[0] == 401
            assert call("/chat/requests/" + first["request_id"] + "/context", origin="https://untrusted.invalid")[0] == 403

            # P1-T14 happy path: read -> edit -> bash -> answer, fully recorded over HTTP.
            happy = submit("rename beta to gamma")
            happy_done = wait_receipt(happy["request_id"], "complete")
            assert "changed beta to gamma" in happy_done["response"]
            assert (root / "notes.md").read_text() == "alpha\ngamma\n"
            happy_detail = call("/chat/requests/" + happy["request_id"] + "/steps")[1]
            happy_steps = happy_detail["steps"]
            assert [(s["kind"], s["status"], s["tool_name"]) for s in happy_steps] == [
                ("model_call", "complete", None), ("tool_call", "complete", "read"),
                ("model_call", "complete", None), ("tool_call", "complete", "edit"),
                ("model_call", "complete", None), ("tool_call", "complete", "bash"),
                ("model_call", "complete", None), ("verification", "complete", None),
            ], happy_steps
            # P5-T01: the answered turn also carries an advisory verification projection. It cites
            # step ids from this turn only, and the verifier itself is a separate text-only call.
            happy_verification = happy_detail["verification"]
            assert happy_verification["status"] == "verified", happy_verification
            assert happy_verification["unverified_claims"] == 0, happy_verification
            assert happy_verification.get("error_code") is None, happy_verification
            assert happy_verification["claims"][0]["evidence_step_ids"][0] in {s["id"] for s in happy_steps}
            verifier_bodies = [item["body"] for item in provider.requests if item["scenario"] == "__verification__"]
            assert verifier_bodies, "an answered turn must be audited"
            assert all("tools" not in body for body in verifier_bodies), "the verifier gets no tools"
            happy_feed = call("/activity?session_id=" + happy["session_id"])[1]["events"]
            happy_kinds = [event["kind"] for event in happy_feed]
            assert happy_kinds.index("verification_started") < happy_kinds.index("verified") < happy_kinds.index("answer_saved"), happy_kinds
            assert any(event["kind"] == "file_changed" for event in happy_feed)
            assert any(event["kind"] == "tool_finished" and event["payload"].get("exit_code") == 0 for event in happy_feed)
            changes = call("/changes?request_id=" + happy["request_id"])[1]["changes"]
            assert len(changes) == 1 and changes[0]["applied"] is True
            happy_requests = [item["body"] for item in provider.requests if item["scenario"] == "rename beta to gamma"]
            assert len(happy_requests) == 4
            assert any(message.get("role") == "tool" for message in happy_requests[1]["messages"])
            assert any(message.get("role") == "tool" and "gamma" in message.get("content", "") for message in happy_requests[2]["messages"])

            # P5-T03: one delegated exploration over the real surface. The sub-agent's own model and
            # tool calls hang off a `subagent` step under the `task` step, it is offered read-only
            # tools only, and the parent receives the bounded report rather than the transcript.
            # The mode here is still auto_all, so a refused `write` proves that the allow-list, not
            # the approval gate, is what keeps a sub-agent read-only.
            explored = submit("delegate the search")
            explored_done = wait_receipt(explored["request_id"], "complete")
            assert explored_done["response"] == "A sub-agent found gamma on line 2."
            assert (root / "deny.md").read_text() == "keep this file\n", "delegation must not reach a side-effecting tool"
            with sqlite3.connect(db) as connection:
                tree = connection.execute(
                    "SELECT s.kind,s.status,s.tool_name,s.error_code,COALESCE(p.seq,-1) FROM turn_steps s "
                    "LEFT JOIN turn_steps p ON p.id=s.parent_step_id WHERE s.request_id=? ORDER BY s.seq",
                    (explored["request_id"],),
                ).fetchall()
            assert tree == [
                ("model_call", "complete", None, None, -1),
                ("tool_call", "complete", "task", None, -1),
                ("subagent", "complete", None, None, 1),
                ("model_call", "complete", None, None, 2),
                ("tool_call", "complete", "read", None, 2),
                ("tool_call", "failed", "write", "unknown_tool", 2),
                ("model_call", "complete", None, None, 2),
                ("model_call", "complete", None, None, -1),
                ("verification", "complete", None, None, -1),
            ], tree
            explored_kinds = [event["kind"] for event in call("/activity?session_id=" + explored["session_id"])[1]["events"]]
            assert explored_kinds.index("subagent_started") < explored_kinds.index("subagent_finished") < explored_kinds.index("answer_saved"), explored_kinds
            sub_bodies = [item["body"] for item in provider.requests if item["scenario"] == explore_key]
            assert len(sub_bodies) == 2, sub_bodies
            assert [tool["function"]["name"] for tool in sub_bodies[0]["tools"]] == ["read", "grep", "glob"], sub_bodies[0]["tools"]
            assert len(sub_bodies[0]["messages"]) == 2, "a sub-agent starts from its own context, not the parent's history"
            explored_requests = [item["body"] for item in provider.requests if item["scenario"] == "delegate the search"]
            report = [message for message in explored_requests[-1]["messages"] if message.get("role") == "tool"][-1]["content"]
            assert "sub-agent report" in report and "notes.md line 2 holds gamma." in report, report
            assert "files read:" in report and "- notes.md" in report, report
            assert "alpha" not in report, "the parent gets the report, never the sub-agent's transcript"

            # P2-T01: the same rows, live. The stream is opened before the turn, so the frames are
            # produced as the loop commits them, and every frame must be the `/activity` row itself.
            stream_session = str(uuid.uuid4())
            live = open_stream("/activity/stream?session_id=" + stream_session)
            streamed_turn = submit("stream a turn", session=stream_session)
            wait_receipt(streamed_turn["request_id"], "complete")
            polled = call("/activity?session_id=" + stream_session)[1]
            assert len(polled["events"]) >= 2, polled
            streamed = read_frames(live, len(polled["events"]))
            live.close()
            assert [(seq, kind) for seq, kind, _ in streamed] == [(event["seq"], event["kind"]) for event in polled["events"]], (streamed, polled)
            assert [row for _, _, row in streamed] == polled["events"], "a frame carries the recorded row, not a second rendering of it"
            # Exactly-once comes from the sequence, not the socket: resuming replays nothing.
            resumed = open_stream(f"/activity/stream?session_id={stream_session}&after_seq={polled['next_after_seq']}", timeout=1.5)
            assert read_frames(resumed, 1, seconds=3) == [], "a resumed stream must not repeat a delivered event"
            resumed.close()
            # The stream keeps the feed's bounds and the same bearer token (never a URL token).
            # A missing `session_id` is an axum rejection with a plain-text body, so its status is
            # asserted in the Rust test instead of here, where `call` expects JSON.
            for path, code in [
                (f"/activity/stream?session_id={stream_session}&after_seq=-1", 400),
                ("/activity/stream?session_id=nope", 400),
            ]:
                assert call(path)[0] == code, path
            assert call("/activity/stream?session_id=" + stream_session, auth=False)[0] == 401

            # Stale hash anchor: the tool refuses the edit and never touches disk.
            stale = submit("stale anchor")
            wait_receipt(stale["request_id"], "complete")
            assert (root / "stale.md").read_text() == "alpha\nbeta\n"
            stale_steps = call("/chat/requests/" + stale["request_id"] + "/steps")[1]["steps"]
            assert any(s["tool_name"] == "edit" and s["error_code"] == "stale_anchor" and s["status"] == "failed" for s in stale_steps)
            assert call("/changes?request_id=" + stale["request_id"])[1] == {"changes": []}

            # Sandbox escape: the read is recorded as a failed tool, with no filesystem escape.
            escape = submit("path escape")
            wait_receipt(escape["request_id"], "complete")
            escape_steps = call("/chat/requests/" + escape["request_id"] + "/steps")[1]["steps"]
            assert any(s["tool_name"] == "read" and s["error_code"] in ("path_denied", "invalid_arguments") for s in escape_steps)

            # Permission deny: the pending row is the only thing that can unblock the write.
            configure(permission_mode="ask")
            denied = submit("deny write")
            deadline = time.time() + 10
            permission = None
            while time.time() < deadline:
                listed = call("/permissions?scope=global")[1]["permissions"]
                permission = next((item for item in listed if item["request_id"] == denied["request_id"]), None)
                if permission:
                    break
                time.sleep(0.05)
            assert permission is not None
            code, decision = call("/permissions/" + permission["id"], {"decision": "deny", "scope": "global"})
            assert code == 200 and decision["status"] == "denied"
            wait_receipt(denied["request_id"], "complete")
            assert (root / "deny.md").read_text() == "keep this file\n"
            denied_steps = call("/chat/requests/" + denied["request_id"] + "/steps")[1]["steps"]
            assert any(s["tool_name"] == "write" and s["status"] == "denied" and s["error_code"] == "denied" for s in denied_steps)

            # Budget exhaustion: one model call is allowed, then the loop answers honestly without
            # making another paid provider call.
            configure(permission_mode="auto_all", max_steps=1)
            budget = submit("budget test")
            budget_done = wait_receipt(budget["request_id"], "complete")
            assert "max_steps" in budget_done["response"] and "NOT finished" in budget_done["response"]
            assert provider.count("budget test") == 1
            assert any(event["kind"] == "budget_exhausted" for event in call("/activity?session_id=" + budget["session_id"])[1]["events"])
            configure(max_steps=40)

            # Crash during a real bash tool: restart marks the running step interrupted and does
            # not replay it or ask the provider for another model call.
            hold = submit("hold during tool")
            wait_db(hold["request_id"], lambda rows: any(row[1] == "tool_call" and row[2] == "running" for row in rows))
            before_restart = provider.count("hold during tool")
            assert app is not None
            app.kill()
            app.wait(timeout=5)
            app = start()
            interrupted = wait_receipt(hold["request_id"], "interrupted")
            assert interrupted["response"] is None
            interrupted_steps = call("/chat/requests/" + hold["request_id"] + "/steps")[1]["steps"]
            assert [(s["kind"], s["status"]) for s in interrupted_steps] == [("model_call", "complete"), ("tool_call", "interrupted")]
            assert provider.count("hold during tool") == before_restart, "restart must not re-execute the bash step"
            assert "interrupted" in [event["kind"] for event in call("/activity?session_id=" + hold["session_id"])[1]["events"]]

            failure_turn = submit("simulate provider failure")
            wait_receipt(failure_turn["request_id"], "failed")
            failed_steps = call("/chat/requests/" + failure_turn["request_id"] + "/steps")[1]["steps"]
            assert [(s["kind"], s["status"], s["error_code"]) for s in failed_steps] == [("model_call", "failed", "provider_failed")]

            with sqlite3.connect(db) as connection:
                assert connection.execute("PRAGMA integrity_check").fetchone()[0] == "ok"
                assert connection.execute("PRAGMA foreign_key_check").fetchall() == []
            print("PASS: P1-T14 tool calls, read/edit/bash/answer, stale anchors, path escape, permission deny, budget exhaustion, interrupted tool recovery, no re-execution, and provider failure")
        finally:
            if app and app.poll() is None:
                app.terminate()
                try:
                    app.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    app.kill()
                    app.wait()
    finally:
      shutil.rmtree(project_tmp, ignore_errors=True)
    provider.close()


if __name__ == "__main__":
    main()
