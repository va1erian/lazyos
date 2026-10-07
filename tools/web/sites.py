"""The host's stand-ins for the sites the LazyWeb harness browses.

This sandbox (and CI) cannot reach example.com or theoldnet.com, so the
harness serves both from the host and the test image's `/etc/hosts` maps
their names to the host as the guest sees it (QEMU's user network: the
gateway 10.0.2.2 is the host's loopback). The browser keeps the real URLs,
so the servers listen on the real ports:

    http://example.com/        127.0.0.1:80   fixtures/example.com/index.html
    http://theoldnet.com/      127.0.0.1:80   301 to https://theoldnet.com/<path>
    https://theoldnet.com/     127.0.0.1:443  fixtures/theoldnet.com/ (and www.)
    https://en.wikipedia.org/  127.0.0.1:443  fixtures/wikipedia/ (`wiki.py`; also
                                              upload. and thumb.wikimedia.org)

The HTTPS server presents a leaf for theoldnet.com, www.theoldnet.com and
the Wikipedia hosts issued by the run's throwaway test CA (`certs.py`), which the image trusts
through `LAZYOS_TLS_TEST_CA`. Every request is recorded with its scheme,
Host header, path, User-Agent and the TLS connection's SNI: that record, not
the guest's word, is the evidence the judge reads (`judge.py`).

    python tools/web/sites.py OUTDIR    # serve by hand (needs ports 80 and 443)
"""

from __future__ import annotations

import socket
import ssl
import sys
import threading
import time
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import wiki

HERE = Path(__file__).resolve().parent
FIXTURES = HERE / "fixtures"
HTTP_PORT, HTTPS_PORT = 80, 443
EXAMPLE_HOSTS = ("example.com", "www.example.com")
OLDNET_HOSTS = ("theoldnet.com", "www.theoldnet.com")
TYPES = {".html": "text/html; charset=utf-8", ".css": "text/css", ".png": "image/png",
         ".gif": "image/gif", ".jpg": "image/jpeg"}
#: A file the browser cannot show, served as an attachment: it downloads it.
DOWNLOAD_PATH = "/files/oldnet-kit.zip"
DOWNLOAD_NAME = "oldnet-kit.zip"


def download_payload() -> bytes:
    """The download's bytes: 300 KB the judge can check by length."""
    return bytes(i % 251 for i in range(300 * 1024))


@dataclass
class Request:
    scheme: str
    host: str
    method: str
    path: str
    status: int
    user_agent: str = ""
    sni: str | None = None


@dataclass
class Record:
    """Every request both servers answered, guarded by one lock."""
    requests: list[Request] = field(default_factory=list)
    #: SNI of every completed TLS handshake, in order.
    handshakes: list[str | None] = field(default_factory=list)
    lock: threading.Lock = field(default_factory=threading.Lock)

    def snapshot(self) -> "Record":
        with self.lock:
            return Record(list(self.requests), list(self.handshakes))


def _host(header: str | None) -> str:
    """The Host header's name, lowercased, without a port."""
    name = (header or "").strip().lower()
    return name.rsplit(":", 1)[0] if name.count(":") == 1 else name


def fixture(site: str, path: str, example_page: str = "index.html") -> Path | None:
    """The file `path` names under a site's fixtures, or None. `/` is the
    index page; nothing outside the site's directory is ever served."""
    root = (FIXTURES / site).resolve()
    relative = path.split("?", 1)[0].lstrip("/") or (example_page if site == "example.com"
                                                      else "index.html")
    candidate = (root / relative).resolve()
    if root not in candidate.parents or not candidate.is_file():
        return None
    return candidate


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"  # keep-alive: a handshake under TCG is slow
    def version_string(self) -> str:
        return "ECS (lazyweb-harness)"

    def log_message(self, *_args) -> None:
        pass  # the record is the log

    def do_HEAD(self) -> None:
        self.do_GET(body=False)

    def do_GET(self, body: bool = True) -> None:
        scheme = "https" if self.server.tls else "http"
        host = _host(self.headers.get("Host"))
        status, headers, payload = self.route(scheme, host)
        record = self.server.record
        with record.lock:
            record.requests.append(Request(scheme, host, self.command, self.path, status,
                                           self.headers.get("User-Agent", ""),
                                           getattr(self.connection, "lazyweb_sni", None)))
        self.send_response(status)
        for name, value in headers:
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        if body:
            self.wfile.write(payload)

    def route(self, scheme: str, host: str) -> tuple[int, list[tuple[str, str]], bytes]:
        script = self.server.script
        if scheme == "http" and script and self.path.split("?")[0] == script[0]:
            # The harness's check script (`session.py`), for any Host.
            return 200, [("Content-Type", "text/plain")], script[1]
        if scheme == "http" and host in OLDNET_HOSTS + wiki.HOSTS:
            # Like the real sites: plain HTTP only redirects to HTTPS.
            return 301, [("Location", f"https://{host}{self.path}")], b""
        if scheme == "https" and host in wiki.HOSTS:
            copy = wiki.lookup(host, self.path)
            if copy is None:
                return 404, [("Content-Type", "text/html")], b"<html><body>404</body></html>\n"
            return 200, [("Content-Type", copy[0]), ("Cache-Control", "max-age=604800")], copy[1]
        if scheme == "https" and host in OLDNET_HOSTS and self.path.split("?")[0] == DOWNLOAD_PATH:
            return 200, [("Content-Type", "application/zip"),
                         ("Content-Disposition", f'attachment; filename="{DOWNLOAD_NAME}"')], \
                download_payload()
        site = ("example.com" if scheme == "http" and host in EXAMPLE_HOSTS
                else "theoldnet.com" if scheme == "https" and host in OLDNET_HOSTS else None)
        found = fixture(site, self.path, self.server.example_page) if site else None
        if found is None:
            return 404, [("Content-Type", "text/html")], b"<html><body>404 Not Found</body></html>\n"
        kind = TYPES.get(found.suffix, "application/octet-stream")
        return 200, [("Content-Type", kind), ("Cache-Control", "max-age=604800")], found.read_bytes()


class Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, port: int, record: Record, context: ssl.SSLContext | None,
                 example_page: str, script: tuple[str, bytes] | None = None,
                 address: str = "127.0.0.1") -> None:
        self.record, self.context, self.tls = record, context, context is not None
        self.example_page, self.script = example_page, script
        self._sni: dict[int, str | None] = {}
        if context is not None:
            context.sni_callback = lambda sock, name, _ctx: self._sni.__setitem__(id(sock), name)
        super().__init__((address, port), Handler)
        self.thread = threading.Thread(target=self.serve_forever, kwargs={"poll_interval": 0.2},
                                       daemon=True)
        self.thread.start()

    def finish_request(self, request, client_address) -> None:
        """TLS per connection, in the connection's own thread, so a client
        that stalls in the handshake cannot hold up the others."""
        if self.context is not None:
            request.settimeout(60)
            try:
                request = self.context.wrap_socket(request, server_side=True)
            except (ssl.SSLError, OSError):
                return  # a refused handshake: no request to record
            request.lazyweb_sni = self._sni.pop(id(request), None)
            with self.record.lock:
                self.record.handshakes.append(request.lazyweb_sni)
            try:
                super().finish_request(request, client_address)
            finally:
                request.close()  # the wrapper owns the socket now
            return
        super().finish_request(request, client_address)

    def close(self) -> None:
        self.shutdown()
        self.server_close()


class Sites:
    """Both stand-in servers, sharing one record."""

    def __init__(self, leaf_cert: Path, leaf_key: Path, example_page: str = "index.html",
                 script: tuple[str, bytes] | None = None,
                 http_port: int = HTTP_PORT, https_port: int = HTTPS_PORT) -> None:
        self.record = Record()
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(leaf_cert, leaf_key)
        context.set_alpn_protocols(["http/1.1"])
        #: (path, body) the plain server hands out for any Host: the check script.
        self.servers = [Server(http_port, self.record, None, example_page, script)]
        try:
            self.servers.append(Server(https_port, self.record, context, example_page))
        except OSError:
            self.close()
            raise

    def snapshot(self) -> Record:
        return self.record.snapshot()

    def close(self) -> None:
        for server in self.servers:
            server.close()


def port_hint(error: OSError) -> str:
    """Why the servers could not start, and what to do about it."""
    if isinstance(error, PermissionError):
        return (f"{error}: ports 80 and 443 need root, or on Linux "
                "`sudo sysctl net.ipv4.ip_unprivileged_port_start=80`")
    return f"{error}: is another web server listening on 127.0.0.1:80 or :443?"


def ports_free(ports=(HTTP_PORT, HTTPS_PORT)) -> list[int]:
    """The ports something on this machine already holds."""
    busy = []
    for port in ports:
        with socket.socket() as probe:
            probe.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            try:
                probe.bind(("127.0.0.1", port))
            except OSError:
                busy.append(port)
    return busy


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    import certs
    files = certs.generate(Path(sys.argv[1]))
    sites = Sites(files.leaf_cert, files.leaf_key)
    print("serving http://example.com/ and https://theoldnet.com/ on 127.0.0.1; Ctrl+C to stop")
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        sites.close()
