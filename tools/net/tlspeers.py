"""The host's side of the TLS evidence (docs/tls-plan.md §8): HTTPS servers
that record every handshake and request, and the negative servers a correct
client must refuse.

QEMU's user network maps the gateway (10.0.2.2) to the host's loopback, and the
test image's hosts file maps `tls.test` there, so `curl https://tls.test:47790/`
in the guest reaches `GOOD_PORT` below. What a server saw (SNI, ALPN, protocol
version, cipher suite, request lines and headers, any application data after a
failed handshake) is the evidence; the guest's own markers only say when it is
done.

    python tools/net/tlspeers.py OUTDIR      # serve by hand (certificates in OUTDIR)
"""

from __future__ import annotations

import gzip
import warnings
import re
import socket
import ssl
import sys
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import tlscerts  # noqa: E402
from hostpeers import pattern  # noqa: E402

#: Good servers: TLS 1.3 (ECDSA), TLS 1.2 only (ECDSA), TLS 1.3 with an RSA
#: chain, and plain HTTP (the target of the refused downgrade, and `http://`).
GOOD_PORT, TLS12_PORT, RSA_PORT, PLAIN_PORT = 47790, 47791, 47792, 47793
#: Servers whose certificate or protocol a client must refuse.
NEGATIVE_PORTS = {
    "expired": 47794,
    "notyet": 47795,
    "wrongname": 47796,
    "selfsigned": 47797,
    "unknownca": 47798,
    "tls10": 47799,
    "cbc": 47800,
    "truncated": 47801,
}

#: Where the plain server hands the guest its check script.
SCRIPT_PATH = "/check.sh"

INDEX = b"LazyOS TLS harness: hello over HTTPS\n" + pattern(3000, seed=0x7151)
CHUNKED = pattern(40_000, seed=0xC4C4)
GZIPPED = (b"compressible line from the TLS harness\n" * 2000)
BIG = pattern(1_048_576, seed=0xB16)
PAGE = b"a page wget saves under its own name\n"
#: Path -> body of every page a good server serves as is.
PAGES = {"/": INDEX, "/big": BIG, "/files/page.txt": PAGE}


@dataclass
class Handshake:
    port: int
    sni: str | None
    alpn: str | None
    version: str
    cipher: str


@dataclass
class Request:
    port: int
    method: str
    path: str
    headers: dict[str, str]


@dataclass
class Record:
    """Everything every server saw, guarded by one lock."""
    handshakes: list[Handshake] = field(default_factory=list)
    requests: list[Request] = field(default_factory=list)
    #: Port -> bytes of application data a negative server received.
    leaked: dict[int, int] = field(default_factory=dict)
    #: Port -> connections accepted (whether or not the handshake completed).
    connections: dict[int, int] = field(default_factory=dict)
    lock: threading.Lock = field(default_factory=threading.Lock)


def _context(leaf: tlscerts.Leaf, role: str) -> ssl.SSLContext:
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.load_cert_chain(leaf.cert, leaf.key)
    context.set_alpn_protocols(["http/1.1"])
    if role == "tls12":
        context.maximum_version = ssl.TLSVersion.TLSv1_2
        context.set_ciphers("ECDHE-ECDSA-AES128-GCM-SHA256")
    elif role == "tls10":
        with warnings.catch_warnings():  # deprecated is the point: the client must refuse it
            warnings.simplefilter("ignore", DeprecationWarning)
            context.minimum_version = context.maximum_version = ssl.TLSVersion.TLSv1
        context.set_ciphers("ECDHE-ECDSA-AES128-SHA:@SECLEVEL=0")
    elif role == "cbc":
        context.maximum_version = ssl.TLSVersion.TLSv1_2
        context.set_ciphers("ECDHE-ECDSA-AES128-SHA256:ECDHE-ECDSA-AES128-SHA")
    return context


def _read_head(conn) -> bytes:
    data = b""
    while b"\r\n\r\n" not in data and len(data) < 65536:
        chunk = conn.recv(4096)
        if not chunk:
            break
        data += chunk
    return data


def _response(status: str, body: bytes, headers: list[tuple[str, str]] = ()) -> bytes:
    lines = [f"HTTP/1.1 {status}", f"Content-Length: {len(body)}", "Connection: close",
             *(f"{k}: {v}" for k, v in headers)]
    return ("\r\n".join(lines) + "\r\n\r\n").encode() + body


def _chunked(body: bytes) -> bytes:
    head = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
    parts = [body[i:i + 4093] for i in range(0, len(body), 4093)]
    return head + b"".join(b"%x\r\n%s\r\n" % (len(p), p) for p in parts) + b"0\r\n\r\n"


def route(path: str, headers: dict[str, str]) -> bytes:
    """The response a good server sends for `path`."""
    if path in PAGES:
        return _response("200 OK", PAGES[path], [("Content-Type", "application/octet-stream")])
    if path == "/chunked":
        return _chunked(CHUNKED)
    if path == "/gzip":
        if "gzip" in headers.get("accept-encoding", ""):
            return _response("200 OK", gzip.compress(GZIPPED, mtime=0), [("Content-Encoding", "gzip")])
        return _response("200 OK", GZIPPED)
    hop = re.fullmatch(r"/redirect/(\d)", path)
    if hop:
        n = int(hop.group(1))
        target = f"/redirect/{n - 1}" if n > 0 else "/"
        return _response("301 Moved Permanently", b"", [("Location", target)])
    if path == "/downgrade":
        location = f"http://{tlscerts.NAME}:{PLAIN_PORT}/downgraded"
        return _response("301 Moved Permanently", b"", [("Location", location)])
    return _response("404 Not Found", b"not here\n")


class Server:
    """One listening port: a good server, a plain one, or a negative one."""

    def __init__(self, port: int, role: str, record: Record, context: ssl.SSLContext | None,
                 extra: dict[str, bytes] | None = None) -> None:
        self.port, self.role, self.record, self.context = port, role, record, context
        #: Pages only this server serves (the plain one hands out the check script).
        self.extra = extra or {}
        self._sni: dict[int, str | None] = {}
        if context is not None:
            context.sni_callback = self._on_sni
        self._sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._sock.bind(("127.0.0.1", port))
        self._sock.listen(16)
        self._sock.settimeout(0.2)
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._accept_loop, daemon=True)
        self._thread.start()

    def _on_sni(self, sock, name, _context) -> None:
        self._sni[id(sock)] = name

    def _accept_loop(self) -> None:
        while not self._stop.is_set():
            try:
                conn, _ = self._sock.accept()
            except (socket.timeout, OSError):
                continue
            with self.record.lock:
                self.record.connections[self.port] = self.record.connections.get(self.port, 0) + 1
            threading.Thread(target=self._serve, args=(conn,), daemon=True).start()

    def _serve(self, conn: socket.socket) -> None:
        conn.settimeout(30)
        try:
            if self.role == "truncated":
                conn.recv(4096)  # the ClientHello; answer with half a record, then hang up
                conn.sendall(b"\x16\x03\x03\x40\x00" + b"\x02" * 20)
                return
            if self.context is not None:
                conn = self.context.wrap_socket(conn, server_side=True)
                version, cipher = conn.version() or "", (conn.cipher() or ("",))[0]
                with self.record.lock:
                    self.record.handshakes.append(Handshake(self.port, self._sni.pop(id(conn), None),
                                                            conn.selected_alpn_protocol(), version, cipher))
            if self.role in NEGATIVE_PORTS:
                self._count_leak(conn)
                return
            self._answer(conn)
        except (ssl.SSLError, OSError):
            pass  # a refused handshake: what the server saw is already recorded
        finally:
            try:
                conn.close()
            except OSError:
                pass

    def _count_leak(self, conn) -> None:
        """A negative server whose handshake completed on its side (TLS 1.3
        finishes before the client checks the chain): count what follows."""
        data = conn.recv(65536)
        with self.record.lock:
            self.record.leaked[self.port] = self.record.leaked.get(self.port, 0) + len(data)

    def _answer(self, conn) -> None:
        head = _read_head(conn).decode("latin-1")
        request_line, _, rest = head.partition("\r\n")
        parts = request_line.split()
        if len(parts) != 3:
            return
        headers = {}
        for line in rest.split("\r\n"):
            name, sep, value = line.partition(":")
            if sep:
                headers[name.strip().lower()] = value.strip()
        with self.record.lock:
            self.record.requests.append(Request(self.port, parts[0], parts[1], headers))
        if parts[1] in self.extra:
            response = _response("200 OK", self.extra[parts[1]])
        else:
            response = route(parts[1], headers)
        if parts[0] == "HEAD":
            response = response.split(b"\r\n\r\n", 1)[0] + b"\r\n\r\n"
        conn.sendall(response)

    def close(self) -> None:
        self._stop.set()
        self._thread.join(timeout=2)
        self._sock.close()


class Peers:
    """Every server the harness runs, sharing one record."""

    def __init__(self, certs: dict, script: bytes = b"") -> None:
        self.record = Record()
        good = certs["good"]
        plan = [(GOOD_PORT, "good", _context(good, "good")),
                (TLS12_PORT, "tls12", _context(good, "tls12")),
                (RSA_PORT, "rsa", _context(certs["rsa"], "rsa")),
                (PLAIN_PORT, "plain", None)]
        for role, port in NEGATIVE_PORTS.items():
            leaf = certs.get(role, good)
            plan.append((port, role, None if role == "truncated" else _context(leaf, role)))
        self.servers = [Server(port, role, self.record, context,
                               {SCRIPT_PATH: script} if role == "plain" else None)
                        for port, role, context in plan]

    def snapshot(self) -> Record:
        with self.record.lock:
            return Record(list(self.record.handshakes), list(self.record.requests),
                          dict(self.record.leaked), dict(self.record.connections))

    def close(self) -> None:
        for server in self.servers:
            server.close()


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    peers = Peers(tlscerts.generate(Path(sys.argv[1])))
    print("serving; Ctrl+C to stop")
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        peers.close()
