#!/usr/bin/env python3
"""A mock Fireworks over TLS for LazyAgent's harness (docs/agent.md).

One HTTPS server answers as `api.fireworks.ai`: `POST
/inference/v1/chat/completions` with a streamed Server-Sent Events reply. The
reply depends on the last user message, so one session can exercise the app:

* anything: a Markdown answer (a heading, a list, a quote, a code block and a
  link) after a few streamed reasoning chunks;
* `STALL`: a few words, then silence (the session presses Escape);
* `LONG`: forty paragraphs (the session scrolls up);
* a title request (the app asks for one after the first answer): a short title.

A request whose `Authorization` is not `Bearer <the expected key>` gets a 401
like Fireworks'. Every request is recorded (method, path, Host, SNI, headers,
body) for the judge, which checks what the guest really sent.

    python tools/agent/mockserver.py CERT KEY [--port 443] [--key KEY]
"""

from __future__ import annotations

import argparse
import json
import ssl
import sys
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path

PORT = 443
HOST = "api.fireworks.ai"
PATH = "/inference/v1/chat/completions"
#: Seconds between streamed chunks, so the reply is seen arriving.
PACE = 0.03

ANSWER = (
    "## What a DHCP lease is\n\n"
    "A **lease** is the address a server lends you for a while. See "
    "[the RFC](https://www.rfc-editor.org/rfc/rfc2131) for details.\n\n"
    "- the client asks\n- the server offers\n- the client renews at half the time\n\n"
    "> Renewal is quiet when it works.\n\n"
    "```sh\nip addr show dev eth0\n```\n\nThat is all."
)
REASONING = "The user asks about DHCP leases. A short structured answer will do."


@dataclass
class Request:
    method: str
    path: str
    host: str
    sni: str
    headers: dict
    body: str


@dataclass
class Record:
    requests: list[Request] = field(default_factory=list)


def sse(obj: dict) -> bytes:
    return f"data: {json.dumps(obj)}\n\n".encode()


def chunk(data: bytes) -> bytes:
    return f"{len(data):x}\r\n".encode() + data + b"\r\n"


def delta(text: str = "", reasoning: str = "", finish: str | None = None) -> dict:
    d: dict = {}
    if text:
        d["content"] = text
    if reasoning:
        d["reasoning_content"] = reasoning
    return {"choices": [{"index": 0, "delta": d, "finish_reason": finish}]}


def pieces(text: str, size: int = 28) -> list[str]:
    return [text[i:i + size] for i in range(0, len(text), size)]


def answer_for(prompt: str) -> tuple[str, str, bool]:
    """`(reasoning, text, stall)` for the user's last message."""
    if "STALL" in prompt:
        return "", "Once upon a time, ", True
    if "LONG" in prompt:
        return "", "".join(f"Paragraph number {n} says something useful about leases.\n\n" for n in range(1, 41)), False
    return REASONING, ANSWER, False


class Mock:
    def __init__(self, cert: Path, key: Path, port: int, host: str, expected_key: str):
        self.record = Record()
        self.expected = expected_key
        self._lock = threading.Lock()
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(str(cert), str(key))
        context.set_alpn_protocols(["http/1.1"])
        context.sni_callback = self._on_sni
        self._context = context
        import socket
        self._listener = socket.socket()
        self._listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._listener.bind((host, port))
        self._listener.listen(16)
        self._closed = False
        self._thread = threading.Thread(target=self._accept, daemon=True)
        self._thread.start()

    @staticmethod
    def _on_sni(sock, name, _context):
        sock.sni = name or ""

    def close(self) -> None:
        self._closed = True
        try:
            self._listener.close()
        except OSError:
            pass

    def _accept(self) -> None:
        while not self._closed:
            try:
                conn, _ = self._listener.accept()
            except OSError:
                return
            threading.Thread(target=self._serve, args=(conn,), daemon=True).start()

    def _serve(self, raw) -> None:
        try:
            raw.settimeout(30)
            conn = self._context.wrap_socket(raw, server_side=True)
        except (OSError, ssl.SSLError):
            raw.close()
            return
        try:
            self._handle(conn)
        except (OSError, ssl.SSLError):
            pass
        finally:
            try:
                conn.close()
            except OSError:
                pass

    def _read(self, conn) -> Request | None:
        data = b""
        while b"\r\n\r\n" not in data:
            piece = conn.recv(4096)
            if not piece:
                return None
            data += piece
        head, _, rest = data.partition(b"\r\n\r\n")
        lines = head.decode("latin-1").split("\r\n")
        method, path, _ = lines[0].split(" ", 2)
        headers = {}
        for line in lines[1:]:
            name, _, value = line.partition(":")
            headers[name.strip().lower()] = value.strip()
        length = int(headers.get("content-length", "0") or 0)
        while len(rest) < length:
            piece = conn.recv(65536)
            if not piece:
                break
            rest += piece
        sni = getattr(conn, "sni", "") or ""
        return Request(method, path, headers.get("host", ""), sni, headers, rest[:length].decode("utf-8", "replace"))

    def _handle(self, conn) -> None:
        request = self._read(conn)
        if request is None:
            return
        with self._lock:
            self.record.requests.append(request)
        if request.method != "POST" or request.path != PATH:
            self._plain(conn, 404, {"error": {"message": "not found"}})
            return
        if request.headers.get("authorization") != f"Bearer {self.expected}":
            self._plain(conn, 401, {"error": {"message": "The API key you provided is invalid.",
                                              "code": "UNAUTHORIZED", "type": "error"}})
            return
        try:
            body = json.loads(request.body)
            messages = body["messages"]
        except (ValueError, KeyError):
            self._plain(conn, 400, {"error": {"message": "bad request"}})
            return
        system = messages[0].get("content", "") if messages else ""
        prompt = next((m["content"] for m in reversed(messages) if m.get("role") == "user"), "")
        if "title of at most six words" in str(system):
            self._stream(conn, "", "Leases explained", False)
            return
        reasoning, text, stall = answer_for(str(prompt))
        self._stream(conn, reasoning, text, stall)

    def _plain(self, conn, status: int, obj: dict) -> None:
        payload = json.dumps(obj).encode()
        conn.sendall(f"HTTP/1.1 {status} X\r\nContent-Type: application/json\r\n"
                     f"Content-Length: {len(payload)}\r\nConnection: close\r\n\r\n".encode() + payload)

    def _stream(self, conn, reasoning: str, text: str, stall: bool) -> None:
        conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n"
                     b"Fireworks-Server-Time-To-First-Token: 0.200\r\nConnection: close\r\n\r\n")
        for part in pieces(reasoning, 24):
            conn.sendall(chunk(sse(delta(reasoning=part))))
            time.sleep(PACE)
        for part in pieces(text):
            conn.sendall(chunk(sse(delta(text=part))))
            time.sleep(PACE)
        if stall:
            # Silence until the client gives up (Stop closes the connection).
            conn.settimeout(60)
            try:
                conn.recv(1)
            except (OSError, ssl.SSLError):
                pass
            return
        conn.sendall(chunk(sse(delta(finish="stop"))))
        conn.sendall(chunk(sse({"choices": [], "usage": {
            "prompt_tokens": 40, "completion_tokens": max(1, len(text) // 4),
            "completion_tokens_details": {"reasoning_tokens": len(reasoning) // 4}}})))
        conn.sendall(chunk(b"data: [DONE]\n\n"))
        conn.sendall(b"0\r\n\r\n")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("cert")
    parser.add_argument("key")
    parser.add_argument("--port", type=int, default=PORT)
    parser.add_argument("--key-value", default="test-key", dest="expected")
    args = parser.parse_args()
    mock = Mock(Path(args.cert), Path(args.key), args.port, "127.0.0.1", args.expected)
    print(f"mock Fireworks on 127.0.0.1:{args.port}; ^C to stop", file=sys.stderr)
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        mock.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
