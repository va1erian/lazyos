"""Client for `dbgd`, the remote inspection service of a LazyOS box
(docs/dbgd-plan.md): newline-delimited JSON-RPC over TCP behind a
pre-shared-key handshake.

    with DbgClient("192.168.1.50", 9701, key_hex) as dbg:
        print(dbg.call("tasks.list"))
        dbg.follow(lambda line: print(line["text"]), seconds=30)

The key is the hex string the image was built with (`LAZYOS_DBGD_KEY`, or
`target/dbgd.key` when the build made one): `default_key()` reads that file.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import socket
import time
from pathlib import Path
from typing import Callable

PROTO = "lazyos-dbg/1"
DEFAULT_PORT = 9701
#: Longest reply line accepted (the largest legitimate one is a 32 KiB file
#: read in JSON, well under this).
MAX_LINE = 4 << 20
ROOT = Path(__file__).resolve().parents[2]


class DbgError(Exception):
    """A JSON-RPC error answer (or a broken handshake)."""

    def __init__(self, code: int, message: str):
        super().__init__(f"[{code}] {message}")
        self.code = code
        self.message = message


def default_key() -> str | None:
    """The key the last build with `LAZYOS_DBGD=1` generated, if any."""
    path = ROOT / "target" / "dbgd.key"
    try:
        return path.read_text().strip() or None
    except OSError:
        return None


def client_mac(key: bytes, nonce: bytes) -> str:
    return hmac.new(key, f"{PROTO} client".encode() + nonce, hashlib.sha256).hexdigest()


def server_mac(key: bytes, nonce: bytes) -> str:
    return hmac.new(key, f"{PROTO} server".encode() + nonce, hashlib.sha256).hexdigest()


class DbgClient:
    def __init__(self, host: str, port: int = DEFAULT_PORT, key_hex: str | None = None,
                 timeout: float = 15.0):
        self.host, self.port, self.timeout = host, port, timeout
        self.key = bytes.fromhex(key_hex if key_hex is not None else (default_key() or ""))
        self.sock: socket.socket | None = None
        self.buffer = b""
        self.next_id = 1
        self.hello: dict = {}
        #: Notifications that arrived while a call was waiting for its answer.
        self.pending: list[dict] = []

    # -- connection -------------------------------------------------------

    def __enter__(self) -> "DbgClient":
        self.connect()
        return self

    def __exit__(self, *_exc) -> None:
        self.close()

    def open(self) -> dict:
        """Connect and read the `hello`, without authenticating."""
        self.sock = socket.create_connection((self.host, self.port), timeout=self.timeout)
        self.sock.settimeout(self.timeout)
        self.buffer = b""
        message = self.read_message()
        if message.get("method") != "hello":
            raise DbgError(-1, f"expected hello, got {message}")
        self.hello = message["params"]
        return self.hello

    def connect(self) -> dict:
        """Connect and authenticate; verifies the server's proof too."""
        hello = self.open()
        if hello.get("proto") != PROTO:
            raise DbgError(-1, f"unsupported protocol {hello.get('proto')!r}")
        nonce = bytes.fromhex(hello["nonce"])
        answer = self.call("auth", mac=client_mac(self.key, nonce))
        if not hmac.compare_digest(answer.get("server_mac", ""), server_mac(self.key, nonce)):
            raise DbgError(-1, "the server did not prove it holds the key")
        return hello

    def close(self) -> None:
        if self.sock is not None:
            try:
                self.sock.close()
            finally:
                self.sock = None

    # -- wire -------------------------------------------------------------

    def send_line(self, text: str) -> None:
        assert self.sock is not None
        self.sock.sendall(text.encode() + b"\n")

    def read_message(self, timeout: float | None = None) -> dict:
        """The next line as JSON; raises `socket.timeout` when none arrives."""
        assert self.sock is not None
        self.sock.settimeout(timeout if timeout is not None else self.timeout)
        while b"\n" not in self.buffer:
            data = self.sock.recv(65536)
            if not data:
                raise DbgError(-1, "connection closed by the server")
            self.buffer += data
            if b"\n" not in self.buffer and len(self.buffer) > MAX_LINE:
                raise DbgError(-1, "the server sent a line over the size limit")
        line, _, self.buffer = self.buffer.partition(b"\n")
        return json.loads(line)

    def call(self, method: str, **params) -> dict:
        """Call `method`; the `result`, or `DbgError` for an error answer."""
        ident = self.next_id
        self.next_id += 1
        self.send_line(json.dumps({"jsonrpc": "2.0", "id": ident, "method": method,
                                   "params": params}))
        while True:
            message = self.read_message()
            if "id" not in message:
                self.pending.append(message)
                continue
            if message["id"] != ident:
                continue
            if "error" in message:
                raise DbgError(message["error"]["code"], message["error"]["message"])
            return message["result"]

    def notifications(self, seconds: float) -> list[dict]:
        """Notifications arriving within `seconds` (those already queued too)."""
        out, self.pending = self.pending, []
        end = time.monotonic() + seconds
        while (left := end - time.monotonic()) > 0:
            try:
                message = self.read_message(timeout=left)
            except socket.timeout:
                break
            if "id" not in message:
                out.append(message)
        return out

    def follow(self, on_line: Callable[[dict], None], seconds: float, backlog: int = 0,
               source: str | None = None) -> None:
        """Stream a log ring (`source` "kernel" or "programs") to `on_line`,
        one record per line, as it arrives, for `seconds`, after `backlog`
        lines of history."""
        params = {"lines": backlog}
        if source:
            params["source"] = source
        answer = self.call("log.follow", **params)
        for record in answer.get("lines", []):
            on_line(record)
        end = time.monotonic() + seconds
        pending, self.pending = self.pending, []
        while True:
            for note in pending:
                if note.get("method") == "log":
                    for record in note["params"].get("lines", []):
                        on_line(record)
            pending = []
            left = end - time.monotonic()
            if left <= 0:
                return
            try:
                message = self.read_message(timeout=min(left, 1.0))
            except socket.timeout:
                continue
            if "id" not in message:
                pending.append(message)
