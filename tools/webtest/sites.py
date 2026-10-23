"""The host's stand-ins for the sites the browser harnesses (`tools/lazyweb`) browse.

This sandbox (and CI) cannot reach example.com or theoldnet.com, so the
harness serves both from the host and the test image's `/etc/hosts` maps
their names to the host as the guest sees it (QEMU's user network: the
gateway 10.0.2.2 is the host's loopback). The browser keeps the real URLs,
so the servers listen on the real ports:

    http://example.com/        127.0.0.1:80   fixtures/example.com/index.html
    http://theoldnet.com/      127.0.0.1:80   301 to https://theoldnet.com/<path>
    https://theoldnet.com/     127.0.0.1:443  fixtures/theoldnet.com/ (and www.)

The HTTPS server presents a leaf for theoldnet.com and www.theoldnet.com
(and the harnesses' other test hosts) issued by the run's throwaway test CA (`certs.py`), which the image trusts
through `LAZYOS_TLS_TEST_CA`. Every request is recorded with its scheme,
Host header, path, User-Agent and the TLS connection's SNI: that record, not
the guest's word, is the evidence the judge reads (`tools/lazyweb/judge.py`).

    python tools/webtest/sites.py OUTDIR    # serve by hand (needs ports 80 and 443)
"""

from __future__ import annotations

import html
import os
import socket
import ssl
import sys
import threading
import time
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit


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


#: The WebKit browser harness's form target (its runner, tools/lazyweb/run.py): answers a GET with a page whose
#: title is `echo:<q>`, so what a browser typed into the field comes back as evidence.
ECHO_PATH = "/webkit/echo"
#: A page that answers after `SLOW_SECONDS`: the browser has time to stop the load (the harness
#: presses Stop and judges that a cancelled load is not reported as a failure).
SLOW_PATH = "/webkit/slow"
SLOW_SECONDS = 30
#: LazyWeb's harness (tools/lazyweb): sets the cookie `COOKIE_NAME=COOKIE_VALUE` (a day) and answers
#: with a page whose title is `cookie:<the value of that cookie the request carried, or none>`:
#: the first run's answer is `none`, a run after a restart (a persistent profile) `COOKIE_VALUE`.
COOKIE_PATH = "/lazyweb/cookie"
COOKIE_NAME = "lwp"
COOKIE_VALUE = "kept"
#: LazyWeb's notification harness (tools/lazyweb/notify_run.py): a static page
#: (`fixtures/theoldnet.com/lazyweb/notify.html`, also served on www.theoldnet.com, which is another
#: origin) with three boxes: ask for the permission on a click, post a notification with a `tag` and
#: update it, post six in a burst. A `click` handler sets `document.title`; the page's title says
#: what it learned (`notify:ready:<permission>:<answer without a gesture>`, `notify:perm:<answer>`,
#: `notify:clicked:<title>`).
NOTIFY_PATH = "/lazyweb/notify.html"


#: LazyWeb's ad blocker harness (tools/lazyweb, issue #845): the list host the browser fetches
#: EasyList from (a small list of ours stands in), and the two hosts of the "ad network" its rules
#: name. They answer over HTTPS like the real ones.
LIST_HOST = "easylist.to"
LIST_PATH = "/easylist/easylist.txt"
LIST_ETAG = '"harness-1"'
LIST_MODIFIED = "Tue, 13 Oct 2026 12:47:00 GMT"
AD_HOSTS = ("ads.adnet.test", "tracker.adnet.test")
#: The one script of the ad host the list excepts (`@@||ads.adnet.test/allowed.js`).
AD_ALLOWED_PATH = "/allowed.js"
#: A 1x1 GIF, for the ad host's pictures.
PIXEL = bytes.fromhex("47494638396101000100800000000000ffffff21f90401000000002c00000000010001000002024401003b")


def easylist_text() -> bytes:
    """The harness's list: the ad network and the tracker blocked, one script excepted, one
    element-hiding line (counted and not used), and padding so it is long enough to be a list."""
    lines = [
        "[Adblock Plus 2.0]",
        "! Title: LazyOS harness list",
        "! Expires: 1 hour (update frequency)",
        "||ads.adnet.test^$third-party",
        "||tracker.adnet.test^",
        "@@||ads.adnet.test/allowed.js",
        "example.com##.ad-slot",
        *(f"! padding {n}" for n in range(80)),
    ]
    text = ("\n".join(lines) + "\n").encode()
    big = os.environ.get("LAZYOS_HARNESS_EASYLIST")
    if big:  # a real list in front of the harness's own lines, to try the blocker at full size
        text = Path(big).read_bytes() + b"\n" + text
    return text


def ad_response(path: str) -> tuple[list[tuple[str, str]], bytes]:
    """What an ad host answers: a script for `.js`, a picture for anything else. Never cached, so
    every load reaches the host and the request log is the truth."""
    if path.split("?")[0] == AD_ALLOWED_PATH:
        return [("Content-Type", "application/javascript"), ("Cache-Control", "no-store")], \
            b"window.allowedLoaded = true;\n"
    if path.split("?")[0].endswith(".js"):
        return [("Content-Type", "application/javascript"), ("Cache-Control", "no-store")], \
            b"window.adLoaded = true;\n"
    return [("Content-Type", "image/gif"), ("Cache-Control", "no-store")], PIXEL


def echo_page(path: str) -> bytes:
    """The page `ECHO_PATH` answers with: the query's `q`, escaped, as title and heading."""
    text = html.escape(parse_qs(urlsplit(path).query).get("q", [""])[0])
    return (f"<!doctype html><html><head><meta charset=\"utf-8\"><title>echo:{text}</title></head>"
            f"<body style=\"font:28px sans-serif\"><h1>echo:{text}</h1></body></html>\n").encode()


def cookie_page(cookie_header: str) -> bytes:
    """The page `COOKIE_PATH` answers with: the title says which value of the cookie came."""
    sent = {}
    for part in cookie_header.split(";"):
        name, _, value = part.strip().partition("=")
        if name:
            sent[name] = value
    seen = html.escape(sent.get(COOKIE_NAME, "none"))
    return (f"<!doctype html><html><head><meta charset=\"utf-8\"><title>cookie:{seen}</title></head>"
            f"<body style=\"font:28px sans-serif\"><h1>cookie:{seen}</h1></body></html>\n").encode()


def download_payload() -> bytes:
    """The download's bytes: 300 KB the judge can check by length."""
    return bytes(i % 251 for i in range(300 * 1024))


#: A slow attachment (LazyWeb's harness): `SLOW_DOWNLOAD_BYTES` sent in `SLOW_DOWNLOAD_STEPS` pieces
#: `SLOW_DOWNLOAD_PAUSE` seconds apart, so its progress is on screen while it runs.
SLOW_DOWNLOAD_PATH = "/files/slow-kit.zip"
SLOW_DOWNLOAD_NAME = "slow-kit.zip"
SLOW_DOWNLOAD_BYTES = 200 * 1024
SLOW_DOWNLOAD_STEPS = 10
SLOW_DOWNLOAD_PAUSE = 1.0


def slow_download_payload() -> bytes:
    """The slow download's bytes, checkable by length."""
    return bytes((i * 7) % 251 for i in range(SLOW_DOWNLOAD_BYTES))


@dataclass
class Request:
    scheme: str
    host: str
    method: str
    path: str
    status: int
    user_agent: str = ""
    sni: str | None = None
    #: The Cookie header the request carried (the persistence check of tools/lazyweb).
    cookie: str = ""
    #: `If-None-Match` or `If-Modified-Since`: a conditional request (the list's update check).
    conditional: str = ""


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


def fixture(site: str, path: str) -> Path | None:
    """The file `path` names under a site's fixtures, or None. `/` is the
    index page; nothing outside the site's directory is ever served."""
    root = (FIXTURES / site).resolve()
    relative = path.split("?", 1)[0].lstrip("/") or "index.html"
    candidate = (root / relative).resolve()
    if root not in candidate.parents or not candidate.is_file():
        return None
    return candidate


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"  # keep-alive: a handshake under TCG is slow
    def version_string(self) -> str:
        return "ECS (web-harness)"

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
                                           getattr(self.connection, "harness_sni", None),
                                           self.headers.get("Cookie", ""),
                                           self.headers.get("If-None-Match", "")
                                           or self.headers.get("If-Modified-Since", "")))
        self.send_response(status)
        for name, value in headers:
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        if body and self.path.split("?")[0] == SLOW_DOWNLOAD_PATH:
            step = len(payload) // SLOW_DOWNLOAD_STEPS
            for at in range(0, len(payload), step):
                self.wfile.write(payload[at:at + step])
                self.wfile.flush()
                time.sleep(SLOW_DOWNLOAD_PAUSE)
        elif body:
            self.wfile.write(payload)

    def route(self, scheme: str, host: str) -> tuple[int, list[tuple[str, str]], bytes]:
        if scheme == "http" and host in OLDNET_HOSTS:
            # Like the real sites: plain HTTP only redirects to HTTPS.
            return 301, [("Location", f"https://{host}{self.path}")], b""
        if scheme == "https" and host == LIST_HOST and self.path.split("?")[0] == LIST_PATH:
            headers = [("Content-Type", "text/plain; charset=utf-8"), ("ETag", LIST_ETAG),
                       ("Last-Modified", LIST_MODIFIED)]
            if self.headers.get("If-None-Match") == LIST_ETAG:
                return 304, headers, b""
            return 200, headers, easylist_text()
        if scheme == "https" and host in AD_HOSTS:
            headers, payload = ad_response(self.path)
            return 200, headers, payload
        if scheme == "https" and host in OLDNET_HOSTS and self.path.split("?")[0] == ECHO_PATH:
            return 200, [("Content-Type", "text/html; charset=utf-8"), ("Cache-Control", "no-store")], \
                echo_page(self.path)
        if scheme == "https" and host in OLDNET_HOSTS and self.path.split("?")[0] == COOKIE_PATH:
            return 200, [("Content-Type", "text/html; charset=utf-8"), ("Cache-Control", "no-store"),
                         ("Set-Cookie", f"{COOKIE_NAME}={COOKIE_VALUE}; Max-Age=86400; Path=/")], \
                cookie_page(self.headers.get("Cookie", ""))
        if scheme == "https" and host in OLDNET_HOSTS and self.path.split("?")[0] == SLOW_PATH:
            time.sleep(SLOW_SECONDS)
            return 200, [("Content-Type", "text/html"), ("Cache-Control", "no-store")], \
                b"<!doctype html><title>slow</title><h1>too slow</h1>\n"
        if scheme == "https" and host in OLDNET_HOSTS and self.path.split("?")[0] == SLOW_DOWNLOAD_PATH:
            return 200, [("Content-Type", "application/zip"),
                         ("Content-Disposition", f'attachment; filename="{SLOW_DOWNLOAD_NAME}"')],                 slow_download_payload()
        if scheme == "https" and host in OLDNET_HOSTS and self.path.split("?")[0] == DOWNLOAD_PATH:
            return 200, [("Content-Type", "application/zip"),
                         ("Content-Disposition", f'attachment; filename="{DOWNLOAD_NAME}"')], \
                download_payload()
        site = ("example.com" if scheme == "http" and host in EXAMPLE_HOSTS
                else "theoldnet.com" if scheme == "https" and host in OLDNET_HOSTS else None)
        found = fixture(site, self.path) if site else None
        if found is None:
            return 404, [("Content-Type", "text/html")], b"<html><body>404 Not Found</body></html>\n"
        kind = TYPES.get(found.suffix, "application/octet-stream")
        # The notification page is never cached: a second boot that finds it in the HTTP cache
        # (written by the first) hits a crash in WebKit's NetworkCache::Storage (a thread of the
        # NetworkProcess jumps into padding in `updateFileModificationTime`), which has nothing to
        # do with notifications and is followed up on its own (docs/lazyweb.md, Known limits).
        cache = "no-store" if urlsplit(self.path).path == NOTIFY_PATH else "max-age=604800"
        return 200, [("Content-Type", kind), ("Cache-Control", cache)], found.read_bytes()


class Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, port: int, record: Record, context: ssl.SSLContext | None,
                 address: str = "127.0.0.1") -> None:
        self.record, self.context, self.tls = record, context, context is not None
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
            request.harness_sni = self._sni.pop(id(request), None)
            with self.record.lock:
                self.record.handshakes.append(request.harness_sni)
            try:
                super().finish_request(request, client_address)
            finally:
                request.close()  # the wrapper owns the socket now
            return
        super().finish_request(request, client_address)

    def close(self) -> None:
        self.shutdown()
        self.server_close()


class Closer:
    """A port whose server accepts a connection and hangs up without a word: a client's request
    fails at once ("Empty reply from server"), where a closed port can take seconds to be
    refused (or never, through a user-mode network): the harness's address that must fail."""

    def __init__(self, port: int, address: str = "127.0.0.1") -> None:
        self.listener = socket.socket()
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            self.listener.bind((address, port))
        except OSError:
            self.listener.close()
            raise
        self.listener.listen(8)
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def _serve(self) -> None:
        while True:
            try:
                connection, _peer = self.listener.accept()
            except OSError:
                return  # closed
            connection.close()

    def close(self) -> None:
        self.listener.close()


class Sites:
    """Both stand-in servers, sharing one record (and, with `closer_port`, a port that hangs up)."""

    def __init__(self, leaf_cert: Path, leaf_key: Path,
                 http_port: int = HTTP_PORT, https_port: int = HTTPS_PORT,
                 closer_port: int | None = None) -> None:
        self.record = Record()
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(leaf_cert, leaf_key)
        context.set_alpn_protocols(["http/1.1"])
        self.servers = [Server(http_port, self.record, None)]
        try:
            self.servers.append(Server(https_port, self.record, context))
            if closer_port is not None:
                self.servers.append(Closer(closer_port))
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
