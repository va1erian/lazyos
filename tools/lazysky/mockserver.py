#!/usr/bin/env python3
"""A mock Bluesky over TLS for LazySky's harness (docs/lazysky.md).

One HTTPS server answers as the login service, the PDS and the CDN
(`bsky.test` and `cdn.bsky.test`, the test CA's leaf covers both): sign-in, a
paged timeline of generated posts (links, pictures, quotes, reposts), the
pictures themselves, threads, profiles, notifications, and the repo writes.
Every request is recorded (method, path, Host, SNI, headers, body) for the
judge, which checks what the guest really sent.

Deterministic: `post_view(n, ...)` and the pictures depend only on `n`.

    python tools/lazysky/mockserver.py CERT KEY [--port 8443]   # serve until ^C
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import ssl
import struct
import sys
import threading
import zlib
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

HERE = Path(__file__).resolve().parent


def _load(name: str):
    """A module of `tools/webtest/` by path, without putting that directory on
    `sys.path` (it has a `certs.py` of its own, as this directory does)."""
    spec = importlib.util.spec_from_file_location(f"lazysky_{name}", HERE.parent / "webtest" / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


imgenc = _load("imgenc")
jpegenc = _load("jpegenc")

PORT = 8443
SERVICE_HOST = "bsky.test"
CDN_HOST = "cdn.bsky.test"
ME = "did:plc:meharness"
HANDLE = "me.test"
POSTS = 400
PAGE = 30
#: The picture `big` is served at this size: it proves the client scales a
#: large picture off the UI thread (no JPEG encoder here is fast enough, so
#: it is a PNG written with zlib).
BIG = (3000, 2000)


def base(port: int = PORT) -> str:
    return f"https://{SERVICE_HOST}:{port}"


def cdn(port: int = PORT) -> str:
    return f"https://{CDN_HOST}:{port}"


def post_view(n: int, port: int = PORT) -> dict:
    """The `postView` of post `n`."""
    words = "the quick brown fox jumps over the lazy dog "
    text = f"Post number {n}: " + (words * (1 + n % 5)).strip()
    own = n % 13 == 5
    did = ME if own else "did:plc:aliceharness"
    record = {"text": text, "createdAt": "2026-10-10T12:00:00.000Z"}
    if n % 7 == 0:
        record["facets"] = [{"index": {"byteStart": 0, "byteEnd": 4},
                             "features": [{"$type": "app.bsky.richtext.facet#link",
                                           "uri": f"https://example.test/{n}"}]}]
    view = {
        "uri": f"at://{did}/app.bsky.feed.post/{n}", "cid": f"bafyrei{n:06d}",
        "author": {"did": did, "handle": HANDLE if own else "alice.test",
                   "displayName": "Me" if own else "Alice",
                   "avatar": f"{cdn(port)}/avatar/{n % 4}.jpg"},
        "record": record, "likeCount": n % 40, "repostCount": n % 9, "replyCount": n % 3,
        "indexedAt": "2026-10-10T12:00:01.000Z",
    }
    count = 4 if n % 17 == 3 else (1 if n % 5 == 1 else 0)
    if n == 1:
        count = 1
    if count:
        names = ["big"] if n == 1 else [f"{n}-{i}" for i in range(count)]
        view["embed"] = {"$type": "app.bsky.embed.images#view", "images": [
            {"thumb": f"{cdn(port)}/thumb/{name}.jpg", "fullsize": f"{cdn(port)}/full/{name}.jpg",
             "alt": f"picture {name}", "aspectRatio": {"width": 4, "height": 3}} for name in names]}
    elif n % 7 == 3:
        view["embed"] = {"$type": "app.bsky.embed.record#view", "record": {
            "$type": "app.bsky.embed.record#viewRecord",
            "uri": "at://did:plc:quentin/app.bsky.feed.post/1", "cid": "bafyquote",
            "author": {"did": "did:plc:quentin", "handle": "q.test", "displayName": "Quentin"},
            "value": {"text": "an older thought worth quoting",
                      "createdAt": "2026-10-01T00:00:00.000Z"}}}
    return view


def timeline_page(page: int, port: int = PORT) -> dict:
    feed = []
    for n in range(page * PAGE, min(POSTS, page * PAGE + PAGE)):
        entry = {"post": post_view(n, port)}
        if n % 11 == 0:
            entry["reason"] = {"$type": "app.bsky.feed.defs#reasonRepost",
                               "by": {"did": "did:plc:rita", "handle": "rita.test",
                                      "displayName": "Rita"},
                               "indexedAt": "2026-10-10T13:00:00.000Z"}
        feed.append(entry)
    answer = {"feed": feed}
    if (page + 1) * PAGE < POSTS:
        answer["cursor"] = f"page-{page + 1}"
    return answer


def _gradient(width: int, height: int, tint: int) -> list[tuple[int, int, int]]:
    pixels = []
    for y in range(height):
        for x in range(width):
            stripe = (x + y) % 40 < 5
            pixels.append((255, 255, 255) if stripe else
                          (x * 255 // width, y * 255 // height, (tint * 53) % 200))
    return pixels


_cache: dict[str, bytes] = {}


def picture(kind: str, name: str) -> bytes | None:
    """The bytes of `/thumb/<name>.jpg`, `/full/<name>.jpg` or
    `/avatar/<n>.jpg`; `big` is the large PNG (always as `image/png` bytes)."""
    key = f"{kind}/{name}"
    if key in _cache:
        return _cache[key]
    tint = sum(map(ord, name))
    if name == "big":
        width, height = BIG
        rows = bytearray()
        for y in range(height):
            rows.append(0)
            rows.extend(_big_row(width, y, height))
        header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
        data = (b"\x89PNG\r\n\x1a\n" + imgenc._chunk(b"IHDR", header)
                + imgenc._chunk(b"IDAT", zlib.compress(bytes(rows), 6)) + imgenc._chunk(b"IEND", b""))
    elif kind == "avatar":
        data = jpegenc.jpeg(48, 48, _gradient(48, 48, tint))
    else:
        size = (160, 120) if kind == "thumb" else (320, 240)
        data = jpegenc.jpeg(*size, _gradient(*size, tint))
    _cache[key] = data
    return data


def _big_row(width: int, y: int, height: int) -> bytes:
    green = (y * 255 // height) & 255
    red = bytes(range(256)) * (width // 256 + 1)
    row = bytearray(width * 3)
    row[0::3] = red[:width]
    row[1::3] = bytes([green]) * width
    row[2::3] = bytes([120]) * width
    return bytes(row)


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
    requests: list = field(default_factory=list)
    handshake_names: set = field(default_factory=set)

    def to(self, part: str) -> list:
        return [r for r in self.requests if part in r.path]


class Mock:
    """The server on a thread; `.record` fills as the guest talks to it."""

    def __init__(self, cert: Path, key: Path, port: int = PORT, bind: str = "0.0.0.0"):
        self.port = port
        self.record = Record()
        record, lock = self.record, threading.Lock()
        outer = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_args):
                pass

            def _answer(self, status: int, body: bytes, kind: str = "application/json"):
                self.send_response(status)
                self.send_header("Content-Type", kind)
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(body)

            def _json(self, value, status: int = 200):
                self._answer(status, json.dumps(value).encode())

            def _handle(self):
                length = int(self.headers.get("Content-Length") or 0)
                raw = self.rfile.read(length) if length else b""
                sni = getattr(self.connection, "sni", "") or ""
                text = raw.decode("utf-8", "replace") if len(raw) < 20000 else f"<{len(raw)} bytes>"
                with lock:
                    record.requests.append(Request(
                        self.command, self.path, self.headers.get("Host", ""), sni,
                        {k.lower(): v for k, v in self.headers.items()}, text))
                    if sni:
                        record.handshake_names.add(sni)
                outer.route(self, raw)

            do_GET = do_POST = _handle

        class Server(ThreadingHTTPServer):
            daemon_threads = True
            allow_reuse_address = True

        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(str(cert), str(key))

        def sni(sock, name, _context):
            sock.sni = name or ""

        context.sni_callback = sni
        self.httpd = Server((bind, port), Handler)
        self.httpd.socket = context.wrap_socket(self.httpd.socket, server_side=True)
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)
        self.thread.start()

    def route(self, h: BaseHTTPRequestHandler, raw: bytes) -> None:
        url = urlsplit(h.path)
        path, query = url.path, parse_qs(url.query)
        port = self.port
        picture_match = re.fullmatch(r"/(thumb|full|avatar)/(.+)\.jpg", path)
        if picture_match:
            kind, name = picture_match.groups()
            data = picture(kind, name)
            h._answer(200, data, "image/png" if name == "big" else "image/jpeg")
            return
        if path.endswith("com.atproto.server.createSession"):
            h._json({"accessJwt": "access.harness.jwt", "refreshJwt": "refresh.harness.jwt",
                     "did": ME, "handle": HANDLE,
                     "didDoc": {"service": [{"id": "#atproto_pds",
                                             "serviceEndpoint": base(port)}]}})
            return
        if (h.headers.get("Authorization") or "") != "Bearer access.harness.jwt":
            h._json({"error": "AuthenticationRequired", "message": "no or wrong token"}, 401)
            return
        if path.endswith("app.bsky.feed.getTimeline"):
            cursor = (query.get("cursor") or [""])[0]
            page = int(cursor.split("-")[1]) if cursor.startswith("page-") else 0
            h._json(timeline_page(page, port))
        elif path.endswith("app.bsky.feed.getPostThread"):
            uri = (query.get("uri") or [""])[0]
            n = int(uri.rsplit("/", 1)[-1]) if uri.rsplit("/", 1)[-1].isdigit() else 0
            reply = lambda k: {"$type": "app.bsky.feed.defs#threadViewPost",  # noqa: E731
                               "post": post_view(1000 + k, port), "replies": []}
            h._json({"thread": {"$type": "app.bsky.feed.defs#threadViewPost",
                                "post": post_view(n, port), "replies": [reply(1), reply(2)]}})
        elif path.endswith("com.atproto.repo.createRecord"):
            body = json.loads(raw or b"{}")
            h._json({"uri": f"at://{ME}/{body.get('collection', 'x')}/3kharness",
                     "cid": "bafyharness"})
        elif path.endswith("com.atproto.repo.deleteRecord"):
            h._json({})
        elif path.endswith("app.bsky.notification.getUnreadCount"):
            h._json({"count": 3})
        elif path.endswith("app.bsky.notification.listNotifications"):
            h._json({"notifications": []})
        elif path.endswith("app.bsky.actor.getProfile"):
            h._json({"did": ME, "handle": HANDLE, "displayName": "Me", "followersCount": 12,
                     "followsCount": 3, "postsCount": 40})
        else:
            h._json({"error": "NotFound", "message": f"no mock for {path}"}, 404)

    def close(self) -> None:
        self.httpd.shutdown()
        self.httpd.server_close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("cert")
    parser.add_argument("key")
    parser.add_argument("--port", type=int, default=PORT)
    args = parser.parse_args()
    mock = Mock(Path(args.cert), Path(args.key), args.port)
    print(f"mock Bluesky on {base(args.port)} (^C to stop)", flush=True)
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        pass
    mock.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
