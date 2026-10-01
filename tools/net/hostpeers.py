"""The host's side of the socket evidence (stage N3): echo servers the guest
connects to through the gateway, and a client that connects into the guest.

QEMU's user networking maps the gateway address (10.0.2.2) to the host's
loopback, so a guest `nc 10.0.2.2 47771` reaches `EchoServers.tcp_port` here.
Everything a server receives is recorded, so the harness can compare it with
what the capture shows crossed the wire. The guest's listener is reached the
other way, through the `hostfwd` rule `tools/net/run.py` adds to the netdev.
"""

from __future__ import annotations

import socket
import threading
import time

TCP_PORT = 47771
UDP_PORT = 47772
#: The port the guest's `nc -l` listens on.
GUEST_LISTEN_PORT = 47773


def pattern(n: int, seed: int = 0x5EED) -> bytes:
    """`n` deterministic, non-repeating-looking bytes (an LCG)."""
    out = bytearray(n)
    state = seed
    for i in range(n):
        state = (state * 1103515245 + 12345) & 0x7FFFFFFF
        out[i] = (state >> 16) & 0xFF
    return bytes(out)


class EchoServers:
    """A TCP and a UDP echo server on the loopback, recording what they get."""

    def __init__(self, tcp_port: int = TCP_PORT, udp_port: int = UDP_PORT) -> None:
        self.tcp_streams: list[bytes] = []
        self.udp_datagrams: list[bytes] = []
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._tcp = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._tcp.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._tcp.bind(("127.0.0.1", tcp_port))
        self._tcp.listen(16)
        self._tcp.settimeout(0.2)
        self._udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self._udp.bind(("127.0.0.1", udp_port))
        self._udp.settimeout(0.2)
        self._threads = [
            threading.Thread(target=self._accept_loop, daemon=True),
            threading.Thread(target=self._udp_loop, daemon=True),
        ]
        for thread in self._threads:
            thread.start()

    def _accept_loop(self) -> None:
        while not self._stop.is_set():
            try:
                conn, _ = self._tcp.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            threading.Thread(target=self._serve, args=(conn,), daemon=True).start()

    def _serve(self, conn: socket.socket) -> None:
        received = bytearray()
        conn.settimeout(30)
        try:
            while True:
                chunk = conn.recv(65536)
                if not chunk:
                    break
                received += chunk
                conn.sendall(chunk)
        except OSError:
            pass
        finally:
            conn.close()
            with self._lock:
                self.tcp_streams.append(bytes(received))

    def _udp_loop(self) -> None:
        while not self._stop.is_set():
            try:
                data, peer = self._udp.recvfrom(65536)
            except socket.timeout:
                continue
            except OSError:
                return
            with self._lock:
                self.udp_datagrams.append(data)
            try:
                self._udp.sendto(data, peer)
            except OSError:
                pass

    def snapshot(self) -> tuple[list[bytes], list[bytes]]:
        with self._lock:
            return list(self.tcp_streams), list(self.udp_datagrams)

    def close(self) -> None:
        self._stop.set()
        for sock in (self._tcp, self._udp):
            try:
                sock.close()
            except OSError:
                pass


class InboundProbe:
    """Connect into the guest's listener once it says it is listening, send a
    payload and expect the same bytes back."""

    def __init__(self, forwarded_port: int, payload: bytes) -> None:
        self.port = forwarded_port
        self.payload = payload
        self.echoed = b""
        self.error: str | None = None
        self.done = threading.Event()
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        if self._thread is None:
            self._thread = threading.Thread(target=self._run, daemon=True)
            self._thread.start()

    def _run(self) -> None:
        try:
            deadline = time.time() + 15
            conn = None
            while conn is None:
                try:
                    conn = socket.create_connection(("127.0.0.1", self.port), timeout=10)
                except OSError as exc:
                    if time.time() > deadline:
                        raise
                    time.sleep(0.2)
                    del exc
            conn.settimeout(30)
            sender = threading.Thread(target=lambda: self._send(conn), daemon=True)
            sender.start()
            got = bytearray()
            while len(got) < len(self.payload):
                chunk = conn.recv(65536)
                if not chunk:
                    break
                got += chunk
            self.echoed = bytes(got)
            sender.join(timeout=30)
            conn.shutdown(socket.SHUT_WR)
            while conn.recv(65536):
                pass
            conn.close()
        except OSError as exc:
            self.error = f"{type(exc).__name__}: {exc}"
        finally:
            self.done.set()

    def _send(self, conn: socket.socket) -> None:
        try:
            conn.sendall(self.payload)
        except OSError as exc:
            self.error = f"send: {exc}"

    def verdict(self) -> list[str]:
        problems = []
        if self.error:
            problems.append(self.error)
        if self.echoed != self.payload:
            problems.append(f"the guest echoed {len(self.echoed)} of {len(self.payload)} bytes"
                            + ("" if len(self.echoed) != len(self.payload) else ", and they differ"))
        return problems
