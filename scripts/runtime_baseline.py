#!/usr/bin/env python3
"""Run repeatable, synthetic-provider runtime timing samples against a disposable Harness."""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import resource
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
RUNTIME_SCHEMA = "harness.runtime/v1"
BASELINE_SCHEMA = "harness.runtime-baseline/v1"
STAGES = ("total_ms", "context_ms", "provider_ms", "answer_provider_ms", "verification_provider_ms", "tool_ms", "permission_ms", "verification_ms", "publication_ms")
COUNT_FIELDS = ("provider_calls", "answer_provider_calls", "verification_provider_calls", "tool_calls")
FORBIDDEN_FIELDS = {"prompt", "response", "token", "authorization", "api_key", "path", "arguments", "output", "reasoning"}


def free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def percentile(values: list[int], percent: int) -> int:
    ordered = sorted(values)
    if not ordered:
        raise ValueError("at least one sample is required")
    index = max(0, (len(ordered) * percent + 99) // 100 - 1)
    return ordered[index]


def distribution(values: list[int]) -> dict[str, int]:
    ordered = sorted(values)
    if not ordered:
        raise ValueError("at least one sample is required")
    return {
        "sample_count": len(ordered),
        "median_ms": ordered[(len(ordered) - 1) // 2],
        "p95_ms": percentile(ordered, 95),
        "max_ms": ordered[-1],
    }


def validate_runtime_event(event: dict[str, Any]) -> None:
    if event.get("schema") != RUNTIME_SCHEMA or event.get("event") != "turn_runtime_finished":
        raise ValueError("unexpected runtime event schema")
    if event.get("outcome") != "complete":
        raise ValueError("baseline accepts complete turns only")
    if set(event.get("durations", {})) != set(STAGES):
        raise ValueError("runtime duration fields drifted")
    if set(event.get("counts", {})) != set(COUNT_FIELDS):
        raise ValueError("runtime count fields drifted")
    if event["durations"]["provider_ms"] != event["durations"]["answer_provider_ms"] + event["durations"]["verification_provider_ms"]:
        raise ValueError("provider duration decomposition is inconsistent")
    if event["counts"]["provider_calls"] != event["counts"]["answer_provider_calls"] + event["counts"]["verification_provider_calls"]:
        raise ValueError("provider call decomposition is inconsistent")
    serialized = json.dumps(event, sort_keys=True).lower()
    leaked = sorted(field for field in FORBIDDEN_FIELDS if field in serialized)
    if leaked:
        raise ValueError(f"runtime event contains forbidden fields: {leaked}")


def summarize(events: list[dict[str, Any]], fixture: dict[str, Any], commit: str, db_delta: int, wal_delta: int, cpu_seconds: float, peak_rss_kib: int, environment: dict[str, Any]) -> dict[str, Any]:
    if not events:
        raise ValueError("no runtime events were captured")
    for event in events:
        validate_runtime_event(event)
    stages = {name: distribution([int(event["durations"][name]) for event in events]) for name in STAGES}
    total_median = max(1, stages["total_ms"]["median_ms"])
    for name, summary in stages.items():
        summary["median_total_share_pct"] = round(summary["median_ms"] * 100 / total_median)
    measured = [name for name in STAGES if name != "total_ms"]
    largest = max(measured, key=lambda name: (stages[name]["median_ms"], name))
    largest_provider_purpose = max(
        ("answer_provider_ms", "verification_provider_ms"),
        key=lambda name: (stages[name]["median_ms"], name),
    )
    counts = {}
    for name in COUNT_FIELDS:
        summary = distribution([int(event["counts"][name]) for event in events])
        counts[name] = {
            "sample_count": summary["sample_count"],
            "median": summary["median_ms"],
            "p95": summary["p95_ms"],
            "max": summary["max_ms"],
        }
    fixture_bytes = json.dumps(fixture, sort_keys=True, separators=(",", ":")).encode()
    return {
        "schema": BASELINE_SCHEMA,
        "commit": commit,
        "fixture_sha256": hashlib.sha256(fixture_bytes).hexdigest(),
        "fixture": fixture,
        "sample_count": len(events),
        "stages": stages,
        "counts": counts,
        "largest_measured_stage": largest,
        "largest_provider_purpose": largest_provider_purpose,
        "errors": 0,
        "timeouts": 0,
        "database_bytes_delta": db_delta,
        "wal_bytes_delta": wal_delta,
        "cpu_seconds": round(cpu_seconds, 6),
        "peak_rss_kib": peak_rss_kib,
        "environment": environment,
        "unavailable": sorted(set().union(*(event.get("unavailable", []) for event in events))),
        "limitations": [
            "synthetic loopback provider; no live-provider or network-latency claim",
            "single sequential short-chat scenario on a disposable database",
            "stage medians may overlap and shares must not be summed",
            "SQLite queue/read/write/commit decomposition remains unavailable",
        ],
    }


def file_size(path: Path) -> int:
    try:
        return path.stat().st_size
    except FileNotFoundError:
        return 0


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


class Provider(BaseHTTPRequestHandler):
    delay_seconds = 0.0

    def log_message(self, *_args: object) -> None:
        pass

    def do_GET(self) -> None:
        self.reply({"data": [{"id": "synthetic-model"}]})

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length))
        time.sleep(self.delay_seconds)
        messages = body.get("messages", [])
        if any("HARNESS_VERIFICATION_V1" in str(message.get("content", "")) for message in messages):
            text = json.dumps({"claims": [], "skipped_diagnostics": []})
        elif messages and str(messages[0].get("content", "")).startswith("Extract at most"):
            text = "[]"
        else:
            text = "Synthetic baseline answer"
        self.reply({"choices": [{"message": {"content": text}}]})

    def reply(self, body: dict[str, Any]) -> None:
        payload = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def run(samples: int, provider_delay_ms: int, binary: Path) -> dict[str, Any]:
    if samples < 5 or samples > 200:
        raise ValueError("samples must be between 5 and 200")
    if provider_delay_ms < 0 or provider_delay_ms > 5000:
        raise ValueError("provider delay must be between 0 and 5000 ms")
    if not binary.is_file():
        raise FileNotFoundError(f"build first: {binary}")

    fixture = {
        "scenario": "short_chat",
        "samples": samples,
        "provider_delay_ms": provider_delay_ms,
        "provider": "synthetic_loopback",
        "database": "disposable_sqlite",
        "concurrency": 1,
    }
    Provider.delay_seconds = provider_delay_ms / 1000
    provider = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    thread = threading.Thread(target=provider.serve_forever, daemon=True)
    thread.start()
    server_port = free_port()
    app: subprocess.Popen[bytes] | None = None
    usage_before = resource.getrusage(resource.RUSAGE_CHILDREN)

    with tempfile.TemporaryDirectory(prefix="harness-runtime-baseline-") as tmp:
        tmp_path = Path(tmp)
        db = tmp_path / "baseline.db"
        log = tmp_path / "server.log"
        synthetic_token = "synthetic-baseline-" + "x" * 40
        # A disposable benchmark must not inherit production Harness configuration.
        # In particular, producer credentials are validated against the synthetic owner
        # token and archive paths/keys belong to a different database. Keep ordinary
        # process environment (PATH, locale, etc.) but rebuild the Harness namespace.
        env = {key: value for key, value in os.environ.items() if not key.startswith("HARNESS_")}
        env.update({
            "HARNESS_DB": str(db),
            "HARNESS_AUTH_TOKEN": synthetic_token,
            "HARNESS_API_KEY": "synthetic",
            "HARNESS_BASE_URL": f"http://127.0.0.1:{provider.server_port}",
            "HARNESS_ADDR": f"127.0.0.1:{server_port}",
            "HARNESS_MODEL": "synthetic-model",
        })

        def call(endpoint: str, body: dict[str, Any] | None = None) -> tuple[int, dict[str, Any]]:
            request = urllib.request.Request(
                f"http://127.0.0.1:{server_port}{endpoint}",
                data=None if body is None else json.dumps(body).encode(),
                headers={"Authorization": "Bearer " + synthetic_token, "Content-Type": "application/json"},
                method="POST" if body is not None else "GET",
            )
            try:
                with urllib.request.urlopen(request, timeout=10) as response:
                    return response.status, json.load(response)
            except urllib.error.HTTPError as error:
                with error:
                    return error.code, json.loads(error.read())

        try:
            with log.open("wb") as log_handle:
                app = subprocess.Popen(
                    [str(binary)],
                    cwd=tmp_path,
                    env=env,
                    stdout=subprocess.DEVNULL,
                    stderr=log_handle,
                )
            for _ in range(100):
                if app.poll() is not None:
                    detail = log.read_text(errors="replace")[-4000:].strip()
                    raise RuntimeError(
                        "Harness exited during baseline startup"
                        + (f": {detail}" if detail else "")
                    )
                try:
                    if call("/health")[0] == 200:
                        break
                except urllib.error.URLError:
                    pass
                time.sleep(0.05)
            else:
                raise TimeoutError("Harness baseline startup timed out")

            db_before = file_size(db)
            wal_before = file_size(Path(str(db) + "-wal"))
            for index in range(samples):
                request_id = str(uuid.uuid4())
                body = {
                    "request_id": request_id,
                    "session_id": str(uuid.uuid4()),
                    "scope": "runtime-baseline",
                    "prompt": f"synthetic baseline turn {index}",
                }
                status, receipt = call("/chat/submit", body)
                if status not in (200, 202):
                    raise RuntimeError((status, receipt))
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    status, receipt = call("/chat/requests/" + request_id)
                    if status == 200 and receipt.get("state") == "complete":
                        break
                    if status == 200 and receipt.get("state") in {"failed", "interrupted"}:
                        raise RuntimeError(("turn failed", receipt.get("state")))
                    time.sleep(0.01)
                else:
                    raise TimeoutError("baseline turn timed out")

            if app is not None:
                app.terminate()
                app.wait(timeout=10)
                app = None
            events = []
            for line in log.read_text().splitlines():
                try:
                    value = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if value.get("schema") == RUNTIME_SCHEMA:
                    events.append(value)
            if len(events) != samples:
                raise RuntimeError(f"expected {samples} runtime events, captured {len(events)}")
            usage_after = resource.getrusage(resource.RUSAGE_CHILDREN)
            commit = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
            environment = {
                "python": platform.python_version(),
                "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
                "os": platform.system(),
                "os_release": platform.release(),
                "architecture": platform.machine(),
                "binary_sha256": sha256_file(binary),
                "binary_size_bytes": file_size(binary),
            }
            return summarize(
                events,
                fixture,
                commit,
                file_size(db) - db_before,
                file_size(Path(str(db) + "-wal")) - wal_before,
                usage_after.ru_utime + usage_after.ru_stime - usage_before.ru_utime - usage_before.ru_stime,
                int(usage_after.ru_maxrss),
                environment,
            )
        finally:
            if app is not None and app.poll() is None:
                app.terminate()
                try:
                    app.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    app.kill()
                    app.wait()
            provider.shutdown()
            provider.server_close()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--samples", type=int, default=25)
    parser.add_argument("--provider-delay-ms", type=int, default=20)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/debug/harness")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    report = run(args.samples, args.provider_delay_ms, args.binary)
    encoded = json.dumps(report, sort_keys=True, indent=2) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(encoded)
    print(encoded, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
