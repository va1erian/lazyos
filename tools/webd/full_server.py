"""The web server of the full-view harness (`full_run.py`): 127.0.0.1:18081, which the guest reaches as
10.0.2.2:18081 (QEMU's user network). It serves the pages `webdbrowse`'s scenario expects
(`xui-app/crates/webd-client/src/browse/`) and records every request with its `Cookie` header, so the
judge can tell what each profile sent and what the content blocker kept from the network.

| Path | What it is |
|---|---|
| `/` | "Index": sets `webd=browser`, links to `/page2`, a form, `/pic.png` and `/blocked.png` (the blocker's) |
| `/page2` | "Page Two" |
| `/cookie` | its title is `cookie=<the Cookie header>` |
| `/setcookie-b` | sets `webd=own` |
| `/file.bin` | an attachment of [`FILE_BYTES`] bytes (a download) |
| `/alert` | an alert, then a confirm, then the title `confirm=<answer>` |
| `/popup` | `window.open('/page2')` on load |
| `/mailto` | `location = 'mailto:test@example.com'` on load |
| `/long` | a page 6000 px tall in coloured bands (the wheel scrolls it) |
| `/pic.png`, `/blocked.png`, `/nonet.png` | a small picture (`/nonet.png` must never be asked for) |

It binds no port the other web harnesses use (80, 81, 443; the light harness's 18080).
"""

from __future__ import annotations

import socket
import struct
import threading
import time
import zlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HOST = "127.0.0.1"
PORT = 18081
#: The download: a fixed pattern, so the judge can compare the file the guest saved.
FILE_BYTES = 96 * 1024


def file_body() -> bytes:
    return bytes((i * 7 + i // 251) & 0xFF for i in range(FILE_BYTES))


def png(rgb: tuple[int, int, int], size: int = 16) -> bytes:
    """A `size` x `size` PNG of one colour."""
    raw = b"".join(b"\x00" + bytes(rgb) * size for _ in range(size))

    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    header = struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")


def html(title: str, body: str = "", script: str = "") -> bytes:
    page = (f"<!doctype html><html><head><meta charset=utf-8><title>{title}</title></head>"
            f"<body style='margin:0;font:16px sans-serif'>{body}"
            f"{f'<script>{script}</script>' if script else ''}</body></html>")
    return page.encode()


def long_page() -> bytes:
    colours = ["#c0392b", "#27ae60", "#2980b9", "#f39c12", "#8e44ad", "#16a085"]
    bands = "".join(f"<div style='height:500px;background:{colours[i % len(colours)]};color:#fff;"
                    f"font-size:40px'>band {i}</div>" for i in range(12))
    return html("Long", bands)


INDEX = html(
    "Index",
    "<h1 style='background:#2980b9;color:#fff;margin:0;padding:20px'>Index</h1>"
    "<p><a id=next href='/page2'>Page two</a></p>"
    "<form action='/page2'><input name=q value=x><button>Go</button></form>"
    "<p><img src='/pic.png' width=64 height=64> <img src='/blocked.png' width=64 height=64></p>",
)

PAGES: dict[str, tuple[str, bytes, list[tuple[str, str]]]] = {
    "/": ("text/html", INDEX, [("Set-Cookie", "webd=browser; Path=/")]),
    "/page2": ("text/html", html("Page Two", "<h1>Page Two</h1>"), []),
    "/setcookie-b": ("text/html", html("Set", "<p>set</p>"), [("Set-Cookie", "webd=own; Path=/")]),
    "/file.bin": ("application/octet-stream", file_body(),
                  [("Content-Disposition", 'attachment; filename="file.bin"')]),
    "/alert": ("text/html", html("Alert", "<p>alert</p>",
                                 "alert('hello from the page'); var r = confirm('ok?'); document.title = 'confirm=' + r;"), []),
    "/popup": ("text/html", html("Popup", "<p>popup</p>", "window.open('/page2');"), []),
    "/mailto": ("text/html", html("Mailto", "<p>mailto</p>",
                                  "setTimeout(function () { location = 'mailto:test@example.com'; }, 100);"), []),
    "/long": ("text/html", long_page(), []),
    "/pic.png": ("image/png", png((39, 174, 96)), []),
    "/blocked.png": ("image/png", png((192, 57, 43)), []),
    "/nonet.png": ("image/png", png((0, 0, 0)), []),
}


def busy(port: int = PORT, host: str = HOST) -> bool:
    """Something already listens there (Windows lets a second process bind a listening port, so ask by
    connecting)."""
    with socket.socket() as probe:
        probe.settimeout(0.5)
        return probe.connect_ex((host, port)) == 0


class Server:
    """Serves the pages and records every request: method, path, Cookie, User-Agent, time."""

    def __init__(self, port: int = PORT, host: str = HOST) -> None:
        self.requests: list[dict] = []
        self.lock = lock = threading.Lock()
        requests = self.requests

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def do_GET(self) -> None:  # noqa: N802
                path = self.path.split("?")[0]
                with lock:
                    requests.append({"method": self.command, "path": path, "query": self.path[len(path):],
                                     "cookie": self.headers.get("Cookie", ""),
                                     "agent": self.headers.get("User-Agent", ""), "at": time.time()})
                if path == "/cookie":
                    entry = ("text/html", html("cookie=" + self.headers.get("Cookie", "")), [])
                else:
                    entry = PAGES.get(path)
                if entry is None:
                    self.send_response(404)
                    self.send_header("Content-Length", "0")
                    self.end_headers()
                    return
                kind, body, extra = entry
                self.send_response(200)
                self.send_header("Content-Type", kind)
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Cache-Control", "no-store")
                for name, value in extra:
                    self.send_header(name, value)
                self.end_headers()
                self.wfile.write(body)

            do_HEAD = do_POST = do_GET  # noqa: N815

            def log_message(self, *args) -> None:
                pass

        self.server = ThreadingHTTPServer((host, port), Handler)
        self.server.daemon_threads = True
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def snapshot(self) -> dict:
        with self.lock:
            return {"requests": list(self.requests)}

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()
