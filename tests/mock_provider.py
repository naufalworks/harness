"""Small OpenAI-style loopback provider for the P1 integration suite.

It is intentionally boring: each scenario is a list of canned HTTP responses, selected by the
first user prompt in the request. Every request is retained so tests can assert that tool calls,
assistant messages, and tool results were replayed exactly as sent.
"""
from __future__ import annotations

import json
import threading
from collections import Counter, defaultdict
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any


def text(content: str) -> dict[str, Any]:
    return {"status": 200, "payload": {"choices": [{"message": {"role": "assistant", "content": content}}]}}


def tool_calls(*calls: tuple[str, str, dict[str, Any]], text_content: str | None = None) -> dict[str, Any]:
    return {
        "status": 200,
        "payload": {
            "choices": [{"message": {
                "role": "assistant",
                "content": text_content,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": {"name": name, "arguments": json.dumps(args, separators=(",", ":"))},
                } for call_id, name, args in calls]
            }}],
            "usage": {"prompt_tokens": 11, "completion_tokens": 7},
        },
    }


def failure(status: int, message: str) -> dict[str, Any]:
    return {"status": status, "payload": {"error": {"message": message}}}


class MockProvider:
    def __init__(self, scripts: dict[str, list[dict[str, Any]]]):
        self.scripts = scripts
        self.requests: list[dict[str, Any]] = []
        self.counts: Counter[str] = Counter()
        self._server: ThreadingHTTPServer | None = None
        self._thread: threading.Thread | None = None
        self._lock = threading.Lock()

    @property
    def port(self) -> int:
        assert self._server is not None
        return self._server.server_port

    def count(self, prompt: str) -> int:
        return self.counts[prompt]

    def _scenario(self, body: dict[str, Any]) -> str:
        for message in body.get("messages", []):
            if message.get("role") == "user" and message.get("content") in self.scripts:
                return str(message["content"])
        return "__default__"

    def _next(self, scenario: str) -> dict[str, Any]:
        script = self.scripts.get(scenario) or [text("Synthetic durable answer")]
        index = self.counts[scenario]
        self.counts[scenario] += 1
        return script[min(index, len(script) - 1)]

    def start(self) -> None:
        provider = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args: Any) -> None:
                pass

            def do_POST(self) -> None:  # noqa: N802
                length = int(self.headers.get("Content-Length", "0"))
                body = json.loads(self.rfile.read(length))
                # Background memory extraction is deliberately never scripted as a coding turn.
                if body.get("messages", [{}])[0].get("content", "").startswith("Extract at most"):
                    reply = text("[]")
                    scenario = "__extraction__"
                else:
                    scenario = provider._scenario(body)
                    reply = provider._next(scenario)
                with provider._lock:
                    provider.requests.append({"scenario": scenario, "body": body})
                payload = json.dumps(reply["payload"]).encode()
                try:
                    self.send_response(reply["status"])
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(payload)))
                    self.end_headers()
                    self.wfile.write(payload)
                except (BrokenPipeError, ConnectionResetError):
                    pass

        self._server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    def close(self) -> None:
        if self._server is not None:
            self._server.shutdown()
            self._server.server_close()
            self._server = None
        if self._thread is not None:
            self._thread.join(timeout=5)
            self._thread = None
