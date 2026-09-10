"""Deterministic standard-library CDP WebSocket fixture for the HTTP release gate."""
from __future__ import annotations

import base64
import hashlib
import json
import socket
import struct
import threading
from typing import Any

UNTRUSTED_FIXTURE_TEXT = "UNTRUSTED_FIXTURE_TEXT"
_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
_MAX_FRAME = 2 * 1024 * 1024


def _nodes(revision: int, typed: str = "") -> list[dict[str, Any]]:
    nodes: list[dict[str, Any]] = [
        {
            "nodeId": "1",
            "ignored": False,
            "role": {"value": "RootWebArea"},
            "name": {"value": "Fixture"},
            "backendDOMNodeId": 1,
            "childIds": ["2", "3", "4"],
        },
        {
            "nodeId": "2",
            "parentId": "1",
            "ignored": False,
            "role": {"value": "textbox"},
            "name": {"value": "Name"},
            "value": {"value": typed},
            "backendDOMNodeId": 8,
        },
        {
            "nodeId": "3",
            "parentId": "1",
            "ignored": False,
            "role": {"value": "button"},
            "name": {"value": "Save"},
            "backendDOMNodeId": 7,
        },
        {
            "nodeId": "4",
            "parentId": "1",
            "ignored": False,
            "role": {"value": "StaticText"},
            "name": {"value": UNTRUSTED_FIXTURE_TEXT},
            "backendDOMNodeId": 9,
        },
    ]
    if revision:
        nodes.append(
            {
                "nodeId": "5",
                "parentId": "1",
                "ignored": False,
                "role": {"value": "StaticText"},
                "name": {"value": "Saved"},
                "backendDOMNodeId": 10,
            }
        )
    return nodes


def browser_snapshot_id(url: str, revision: int = 0, typed: str = "") -> str:
    """Mirror browser_tool.rs's bounded canonical snapshot for scripted tool arguments."""
    rows = [
        (0, 1, "RootWebArea", "Fixture", "", "", ""),
        (1, 8, "textbox", "Name", typed, "", ""),
        (1, 7, "button", "Save", "", "", ""),
        (1, 9, "StaticText", UNTRUSTED_FIXTURE_TEXT, "", "", ""),
    ]
    if revision:
        rows.append((1, 10, "StaticText", "Saved", "", "", ""))
    canonical = f"url\0{url}\ntitle\0Fixture\ntotal\0{len(rows)}\n"
    canonical += "".join("\0".join(str(value) for value in row) + "\n" for row in rows)
    return hashlib.sha256(canonical.encode()).hexdigest()[:8]


def _read_exact(sock: socket.socket, length: int) -> bytes:
    output = bytearray()
    while len(output) < length:
        chunk = sock.recv(length - len(output))
        if not chunk:
            raise EOFError("WebSocket peer closed")
        output.extend(chunk)
    return bytes(output)


def _read_frame(sock: socket.socket) -> tuple[int, bytes]:
    head = _read_exact(sock, 2)
    final = bool(head[0] & 0x80)
    opcode = head[0] & 0x0F
    masked = bool(head[1] & 0x80)
    length = head[1] & 0x7F
    if not final or opcode == 0:
        raise AssertionError("fragmented fixture frames are not supported")
    if length == 126:
        length = struct.unpack("!H", _read_exact(sock, 2))[0]
    elif length == 127:
        length = struct.unpack("!Q", _read_exact(sock, 8))[0]
    if length > _MAX_FRAME:
        raise AssertionError("fixture frame exceeded 2 MiB")
    if not masked:
        raise AssertionError("client WebSocket frames must be masked")
    mask = _read_exact(sock, 4)
    payload = bytearray(_read_exact(sock, length))
    for index in range(length):
        payload[index] ^= mask[index % 4]
    return opcode, bytes(payload)


def _send_frame(sock: socket.socket, opcode: int, payload: bytes) -> None:
    length = len(payload)
    if length < 126:
        head = bytes((0x80 | opcode, length))
    elif length <= 0xFFFF:
        head = bytes((0x80 | opcode, 126)) + struct.pack("!H", length)
    else:
        head = bytes((0x80 | opcode, 127)) + struct.pack("!Q", length)
    sock.sendall(head + payload)


class FakeCdp:
    """One loopback endpoint accepting sequential turn-scoped page connections."""

    def __init__(self) -> None:
        self._listener = socket.socket()
        self._listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._listener.bind(("127.0.0.1", 0))
        self._listener.listen()
        self._listener.settimeout(0.25)
        self.port = int(self._listener.getsockname()[1])
        self.endpoint = f"ws://127.0.0.1:{self.port}/devtools/page/fixture"
        self._stop = threading.Event()
        self._lock = threading.Lock()
        self._thread: threading.Thread | None = None
        self.error: BaseException | None = None
        self.requests: list[dict[str, Any]] = []
        self.mouse_releases = 0
        self.typed = ""
        self.revision = 0

    def start(self) -> None:
        self._thread = threading.Thread(target=self._serve, name="fake-cdp", daemon=True)
        self._thread.start()

    def close(self) -> None:
        self._stop.set()
        self._listener.close()
        if self._thread is not None:
            self._thread.join(timeout=5)
            if self._thread.is_alive():
                raise AssertionError("fake CDP server did not stop")
        if self.error is not None:
            raise AssertionError("fake CDP server failed") from self.error

    def dispatch_count(self) -> int:
        with self._lock:
            return self.mouse_releases

    def method_count(self, method: str) -> int:
        with self._lock:
            return sum(request.get("method") == method for request in self.requests)

    def _serve(self) -> None:
        try:
            while not self._stop.is_set():
                try:
                    client, _ = self._listener.accept()
                except socket.timeout:
                    continue
                except OSError:
                    if self._stop.is_set():
                        break
                    raise
                try:
                    self._connection(client)
                finally:
                    client.close()
        except BaseException as error:  # surfaced synchronously from close()
            if not self._stop.is_set():
                self.error = error

    def _connection(self, client: socket.socket) -> None:
        client.settimeout(0.25)
        request = bytearray()
        while b"\r\n\r\n" not in request:
            if len(request) > 16 * 1024:
                raise AssertionError("WebSocket handshake headers too large")
            request.extend(client.recv(4096))
        lines = request.decode("ascii").split("\r\n")
        headers = {
            name.lower(): value.strip()
            for line in lines[1:]
            if ":" in line
            for name, value in [line.split(":", 1)]
        }
        key = headers["sec-websocket-key"]
        accept = base64.b64encode(hashlib.sha1((key + _GUID).encode()).digest()).decode()
        client.sendall(
            (
                "HTTP/1.1 101 Switching Protocols\r\n"
                "Upgrade: websocket\r\n"
                "Connection: Upgrade\r\n"
                f"Sec-WebSocket-Accept: {accept}\r\n\r\n"
            ).encode()
        )

        page_url = "about:blank"
        stale_after_first_snapshot = False
        capture_count = 0
        while not self._stop.is_set():
            try:
                opcode, payload = _read_frame(client)
            except socket.timeout:
                continue
            except (ConnectionError, EOFError, OSError):
                return
            if opcode == 0x8:
                _send_frame(client, 0x8, payload[:125])
                return
            if opcode == 0x9:
                _send_frame(client, 0xA, payload)
                continue
            if opcode != 0x1:
                raise AssertionError(f"unexpected WebSocket opcode {opcode}")
            request_json = json.loads(payload)
            with self._lock:
                self.requests.append(request_json)
            request_id = request_json["id"]
            method = request_json["method"]
            params = request_json.get("params", {})

            if method in ("Page.enable", "Runtime.enable", "DOM.enable", "Accessibility.enable"):
                result: dict[str, Any] = {}
            elif method == "Page.navigate":
                page_url = params["url"]
                stale_after_first_snapshot = page_url.endswith("/stale")
                capture_count = 0
                with self._lock:
                    self.revision = 0
                    self.typed = ""
                result = {"frameId": "frame-1"}
            elif method == "Runtime.evaluate":
                result = {
                    "result": {
                        "type": "object",
                        "value": {"url": page_url, "title": "Fixture"},
                    }
                }
            elif method == "Accessibility.getFullAXTree":
                with self._lock:
                    revision = self.revision
                    typed = self.typed
                result = {"nodes": _nodes(revision, typed)}
                if stale_after_first_snapshot and capture_count == 0:
                    with self._lock:
                        self.revision = 1
                capture_count += 1
            elif method == "DOM.scrollIntoViewIfNeeded":
                result = {}
            elif method == "DOM.getBoxModel":
                result = {
                    "model": {
                        "content": [0.0, 0.0, 100.0, 0.0, 100.0, 40.0, 0.0, 40.0]
                    }
                }
            elif method == "Input.dispatchMouseEvent":
                if params.get("type") == "mouseReleased":
                    with self._lock:
                        self.mouse_releases += 1
                        self.revision = 1
                result = {}
            elif method == "DOM.resolveNode":
                result = {"object": {"objectId": "object-8"}}
            elif method == "Runtime.callFunctionOn":
                with self._lock:
                    self.typed = params["arguments"][0]["value"]
                    self.revision = 1
                result = {"result": {"type": "object", "value": {"ok": True}}}
            elif method in ("Runtime.releaseObject", "Input.dispatchKeyEvent"):
                result = {}
            else:
                response = {
                    "id": request_id,
                    "error": {"code": -32601, "message": f"unknown fixture method {method}"},
                }
                _send_frame(client, 0x1, json.dumps(response, separators=(",", ":")).encode())
                continue

            response = {"id": request_id, "result": result}
            _send_frame(client, 0x1, json.dumps(response, separators=(",", ":")).encode())
