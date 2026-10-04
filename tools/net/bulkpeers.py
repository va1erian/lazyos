"""The host side of the bulk TCP throughput test (docs/performance-plan.md P4).

One TCP server on the loopback, reached by the guest as the gateway. A
client's first line chooses the transfer:

* `PUT <n> <seed>\\n`, then `n` bytes and a half-close: the server checks every
  byte against the stream as it arrives and answers `OK <n> <micros>\\n` (or
  `BAD <why>\\n`);
* `GET <n> <seed>\\n`: the server sends `n` bytes of the stream and closes.

The stream is little-endian 64-bit words `j * MIX + seed`, the same in the
guest's clients (`user/src/bin/netbulk.rs`, `tools/abi/fixtures/src/netbulk.rs`),
so a lost, duplicated or reordered byte is a mismatch at a known offset.
Every transfer is recorded (`Transfer`) with the host's own timing: for a
`PUT` from the first payload byte to the end of the stream, which is the
throughput the wire carried; for a `GET` until the last byte was handed to
the host's socket (the guest's figure is the one to trust there).
"""

from __future__ import annotations

import socket
import struct
import threading
import time
from dataclasses import dataclass, field

MIX = 0x9E3779B97F4A7C15
MASK = (1 << 64) - 1
#: Bytes handed to or taken from the socket at once.
IO = 256 * 1024


def stream(n: int, seed: int) -> bytes:
    """The first `n` bytes of the stream for `seed`."""
    words = (n + 7) // 8
    data = struct.pack(f"<{words}Q", *(((j * MIX) + seed) & MASK for j in range(words)))
    return data[:n]


@dataclass
class Transfer:
    kind: str
    size: int
    seed: int
    received: int = 0
    ok: bool = False
    problem: str = ""
    #: Seconds from the connection's accept to its header line.
    header_s: float = 0.0
    seconds: float = 0.0

    @property
    def mbps(self) -> float:
        return self.size / self.seconds / 1e6 if self.seconds > 0 else 0.0


@dataclass
class BulkServer:
    """The server; `start()` binds and serves until `stop()`."""

    port: int
    transfers: list[Transfer] = field(default_factory=list)

    def __post_init__(self) -> None:
        self._cache: dict[tuple[int, int], bytes] = {}
        self._lock = threading.Lock()
        self._sock: socket.socket | None = None
        self._stop = threading.Event()

    def expected(self, n: int, seed: int) -> bytes:
        with self._lock:
            key = (n, seed)
            if key not in self._cache:
                self._cache[key] = stream(n, seed)
            return self._cache[key]

    def start(self) -> None:
        sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        sock.bind(("127.0.0.1", self.port))
        sock.listen(8)
        sock.settimeout(0.5)
        self._sock = sock
        threading.Thread(target=self._serve, daemon=True).start()

    def stop(self) -> None:
        self._stop.set()
        if self._sock is not None:
            self._sock.close()

    def _serve(self) -> None:
        while not self._stop.is_set():
            try:
                conn, _ = self._sock.accept()
            except (TimeoutError, socket.timeout):
                continue
            except OSError:
                return
            threading.Thread(target=self._handle, args=(conn, time.perf_counter()), daemon=True).start()

    def _handle(self, conn: socket.socket, accepted: float) -> None:
        conn.settimeout(120)
        conn.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 << 20)
        conn.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4 << 20)
        try:
            header, rest = self._header(conn)
            parts = header.split()
            if len(parts) != 3 or parts[0] not in ("PUT", "GET"):
                conn.sendall(b"BAD header\n")
                return
            record = Transfer(parts[0], int(parts[1]), int(parts[2]))
            record.header_s = time.perf_counter() - accepted
            with self._lock:
                self.transfers.append(record)
            if record.kind == "PUT":
                self._put(conn, record, rest)
            else:
                self._get(conn, record)
        except (OSError, ValueError):
            pass
        finally:
            conn.close()

    @staticmethod
    def _header(conn: socket.socket) -> tuple[str, bytes]:
        data = b""
        while b"\n" not in data:
            chunk = conn.recv(256)
            if not chunk:
                raise ValueError("no header")
            data += chunk
            if len(data) > 256:
                raise ValueError("header too long")
        line, rest = data.split(b"\n", 1)
        return line.decode("ascii", "replace"), rest

    def _put(self, conn: socket.socket, record: Transfer, first: bytes) -> None:
        want = self.expected(record.size, record.seed)
        got = 0
        start = None
        chunk = first
        while True:
            if chunk:
                if start is None:
                    start = time.perf_counter()
                end = got + len(chunk)
                if end > record.size:
                    record.problem = f"more than {record.size} bytes"
                    break
                if want[got:end] != chunk:
                    at = next(i for i in range(len(chunk)) if chunk[i] != want[got + i])
                    record.problem = f"byte {got + at} differs"
                    break
                got = end
            chunk = conn.recv(IO)
            if not chunk:
                break
        record.received = got
        record.seconds = time.perf_counter() - (start or time.perf_counter())
        if not record.problem and got != record.size:
            record.problem = f"received {got} of {record.size} bytes"
        record.ok = not record.problem
        answer = f"OK {got} {int(record.seconds * 1e6)}\n" if record.ok else f"BAD {record.problem}\n"
        conn.sendall(answer.encode())

    def _get(self, conn: socket.socket, record: Transfer) -> None:
        data = self.expected(record.size, record.seed)
        start = time.perf_counter()
        view = memoryview(data)
        for at in range(0, len(data), IO):
            conn.sendall(view[at:at + IO])
        conn.shutdown(socket.SHUT_WR)
        # Wait for the guest's close, so `seconds` covers delivery.
        try:
            while conn.recv(IO):
                pass
        except OSError:
            pass
        record.seconds = time.perf_counter() - start
        record.received = len(data)
        record.ok = True
