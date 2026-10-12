#!/usr/bin/env python3
"""The LazyOS netboot registry (docs/netboot-plan.md section 5).

    python tools/netboot/registry.py --root DIR [--bind 0.0.0.0:8420] [--keys keys.json]
                                     [--keep 10] [--log access.log|-] [--public-url URL]

Each flag's default is an environment variable, which is how the container
is configured (see the Dockerfile): `LAZYOS_REGISTRY_BIND` (`0.0.0.0:8420`),
`LAZYOS_REGISTRY_ROOT` (`/srv/lazyos` in the image, else `./registry`),
`LAZYOS_REGISTRY_KEYS` (`<root>/keys.json`), `LAZYOS_REGISTRY_KEEP` (10),
`LAZYOS_REGISTRY_LOG` (`<root>/access.log`, or `-` for stdout) and
`LAZYOS_REGISTRY_URL` (the address iPXE clients should use, when it is not
the `Host` they asked for). `--healthcheck` asks a running server's
`/healthz` and exits 0 or 1 (the image's `HEALTHCHECK`).

Builds, channels, machines, keys: README.md. Standard library only.
"""

from __future__ import annotations

import argparse
import os
import sys
import threading
import time
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))  # `python -I` drops the script directory

import api  # noqa: E402
import httpfiles  # noqa: E402
import store as store_mod  # noqa: E402

DEFAULT_BIND = "0.0.0.0:8420"
DEFAULT_KEEP = 10


def split_bind(bind: str) -> tuple[str, int]:
    """`HOST:PORT` to `(host, port)`; a bare port means every interface, port 0 a free one."""
    host, sep, port = bind.rpartition(":")
    if not sep:
        host, port = "", bind
    if not port.isdigit() or not 0 <= int(port) < 65536:
        raise ValueError(f"bad bind address {bind!r}: use HOST:PORT")
    return host or "0.0.0.0", int(port)


class AccessLog:
    """One line per request, to a file, to stdout (`-`) or nowhere (the tests' list)."""

    def __init__(self, target: str | list | None):
        self._lock = threading.Lock()
        self._list = target if isinstance(target, list) else None
        self._file = None
        if isinstance(target, str) and target != "-":
            Path(target).parent.mkdir(parents=True, exist_ok=True)
            self._file = open(target, "a", buffering=1, encoding="utf-8")
        self._stdout = target == "-"

    def __call__(self, line: str) -> None:
        with self._lock:
            if self._list is not None:
                self._list.append(line)
            if self._file:
                self._file.write(line + "\n")
            if self._stdout:
                print(line, flush=True)

    def close(self) -> None:
        if self._file:
            self._file.close()


def make_handler(app: api.Api, log):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        server_version = "lazyos-registry"

        def log_message(self, *_args) -> None:  # replaced by our own line per request
            pass

        def _finish(self, status: int, sent: int, started: float, note: str = "") -> None:
            seconds = time.monotonic() - started
            rate = f"{sent / seconds / 1e6:.1f} MB/s" if sent > 100_000 and seconds > 0 else ""
            log(f"{store_mod.iso(time.time())} {self.client_address[0]} {self.command} "
                f"{self.path} {status} {sent} B {seconds:.2f} s {rate} {note}".rstrip())

        def _request(self) -> api.Request:
            parts = urllib.parse.urlsplit(self.path)
            segments = [urllib.parse.unquote(s) for s in parts.path.split("/")[1:]]
            if segments == [""]:
                segments = []
            query = dict(urllib.parse.parse_qsl(parts.query))
            length = self.headers.get("Content-Length")
            return api.Request(
                self.command, segments, query, self.headers, self.rfile,
                int(length) if length and length.isdigit() else None,
                self.headers.get("Host", ""), self.client_address[0])

        def _send(self, status: int, headers: dict, body: bytes, started: float, note: str = "") -> None:
            self.send_response(status)
            for key, value in headers.items():
                self.send_header(key, value)
            self.send_header("Content-Length", headers.get("Content-Length", str(len(body))))
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(body)
            self._finish(status, len(body), started, note)

        def _send_file(self, path: Path, started: float) -> None:
            try:
                status, headers, first, length = httpfiles.plan(path, self.headers.get("Range"))
            except httpfiles.RangeError:
                size = path.stat().st_size
                self._send(416, {"Content-Range": f"bytes */{size}"}, b"", started)
                return
            self.send_response(status)
            for key, value in headers.items():
                self.send_header(key, value)
            self.end_headers()
            sent = 0
            if self.command != "HEAD":
                try:
                    sent = httpfiles.copy(path, first, length, self.wfile)
                except (ConnectionError, TimeoutError):
                    self.close_connection = True
                    self._finish(status, sent, started, "(client went away)")
                    return
            self._finish(status, sent, started)

        def _handle(self) -> None:
            started = time.monotonic()
            try:
                response = app.handle(self._request())
            except (ConnectionError, TimeoutError):
                self.close_connection = True
                return
            except Exception as e:  # a bug must not kill the server thread silently
                log(f"{store_mod.iso(time.time())} {self.client_address[0]} {self.command} "
                    f"{self.path} 500 internal error: {type(e).__name__}: {e}")
                response = api.error(500, "internal error")
                self.close_connection = True
            try:
                self._respond(response, started)
            except (ConnectionError, TimeoutError):
                self.close_connection = True  # the client went away (or never read a refusal)

        def _respond(self, response: api.Response, started: float) -> None:
            if response.file is not None:
                self._send_file(response.file, started)
                return
            headers = {"Content-Type": response.content_type, **response.headers}
            self._send(response.status, headers, response.body, started, response.note)
            # An unread request body would be parsed as the next request: drop the connection
            # when a refusal came before the body was consumed.
            if response.status >= 400 and self.command in ("PUT", "POST", "DELETE"):
                self.close_connection = True

        do_GET = do_HEAD = do_PUT = do_POST = do_DELETE = do_PATCH = do_OPTIONS = _handle

    return Handler


def serve(root: Path, bind: str = DEFAULT_BIND, keys_path: Path | None = None,
          keep: int = DEFAULT_KEEP, log=None, public_url: str | None = None,
          clock=time.time) -> ThreadingHTTPServer:
    """A bound server with the layout initialised (call `serve_forever`); port 0 picks a free one."""
    log = log if callable(log) else AccessLog(log)
    store = store_mod.Store(Path(root), keys_path, keep, clock)
    for note in store.init_layout():
        log(f"registry: {note}")
    host, port = split_bind(bind)
    server = ThreadingHTTPServer((host, port), lambda *a, **k: None)
    server.RequestHandlerClass = make_handler(api.Api(store, public_url), log)
    server.daemon_threads = True
    server.store = store
    return server


def healthcheck(bind: str, timeout: float = 3.0) -> int:
    host, port = split_bind(bind)
    url = f"http://{'127.0.0.1' if host in ('0.0.0.0', '') else host}:{port}/healthz"
    try:
        with urllib.request.urlopen(url, timeout=timeout) as r:
            return 0 if r.status == 200 else 1
    except OSError:
        return 1


def main(argv: list[str] | None = None) -> int:
    env = os.environ.get
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--root", default=env("LAZYOS_REGISTRY_ROOT", "registry"),
                    help="the registry's volume (default $LAZYOS_REGISTRY_ROOT or ./registry)")
    ap.add_argument("--bind", default=env("LAZYOS_REGISTRY_BIND", DEFAULT_BIND),
                    help="HOST:PORT (default $LAZYOS_REGISTRY_BIND or %(default)s)")
    ap.add_argument("--keys", default=env("LAZYOS_REGISTRY_KEYS"),
                    help="keys.json (default $LAZYOS_REGISTRY_KEYS or <root>/keys.json)")
    ap.add_argument("--keep", type=int, default=int(env("LAZYOS_REGISTRY_KEEP", DEFAULT_KEEP)),
                    help="builds kept per channel by prune (default $LAZYOS_REGISTRY_KEEP or %(default)s)")
    ap.add_argument("--log", default=env("LAZYOS_REGISTRY_LOG"),
                    help="access log file, or - for stdout (default $LAZYOS_REGISTRY_LOG or <root>/access.log)")
    ap.add_argument("--public-url", default=env("LAZYOS_REGISTRY_URL"),
                    help="the registry's address in the iPXE scripts (default: the request's Host)")
    ap.add_argument("--healthcheck", action="store_true", help="GET /healthz of --bind and exit 0 or 1")
    args = ap.parse_args(argv)
    if args.healthcheck:
        return healthcheck(args.bind)
    if args.keep < 1:
        ap.error("--keep must be at least 1")
    try:
        server = serve(Path(args.root), args.bind, Path(args.keys) if args.keys else None, args.keep,
                       args.log or str(Path(args.root) / "access.log"), args.public_url)
    except (OSError, ValueError) as e:
        print(f"registry: {e}", file=sys.stderr)
        return 2
    print(f"registry: serving {args.root} on {server.server_address[0]}:{server.server_address[1]}",
          flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
