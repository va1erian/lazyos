#!/usr/bin/env python3
"""The Q0 spike's HTTP server: the payload files and the iPXE scripts.

    python tools/netboot/spike/serve.py --root target/netboot/payload \\
        --bind 127.0.0.1:8420 [--guest-host 10.0.2.2]

Serves, from `--root` (what `extract_payload.py` wrote, plus `esp.img` from
`make_esp_image.py` if you made one):

    /boot.ipxe      the menu (docs/netboot-plan.md section 5), one build
    /build.ipxe     imgfetch x3 + chain bootx64.efi
    /sanboot.ipxe   the fallback: sanboot of /files/esp.img
    /files/<name>   a file of the root, `Content-Length` and `Range` honoured
    /               a plain-text index

`--guest-host` is the address the *guest* uses for this server in the scripts
(default 10.0.2.2, which QEMU's user network maps to this machine's loopback).
Every request is logged: client, method, path, status, bytes, seconds and the
rate, which is the download-time number the spike wants. Standard library only.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))  # `python -I` drops the script dir

import common  # noqa: E402
import scripts  # noqa: E402

CHUNK = 256 << 10
TEXT = "text/plain; charset=utf-8"
_RANGE = re.compile(r"^bytes=(\d*)-(\d*)$")


class RangeError(Exception):
    """An unsatisfiable `Range` (answered 416)."""


def parse_range(header: str | None, size: int) -> tuple[int, int] | None:
    """The inclusive `(first, last)` a `Range` header asks of a `size`-byte file.

    `None` for no header or one we ignore (several ranges, other units, bad
    syntax: RFC 9110 says to answer 200 with the whole file). `RangeError`
    when the single range lies beyond the end.
    """
    match = _RANGE.match((header or "").strip())
    if not match or (match[1] == "" and match[2] == ""):
        return None
    first, last = match[1], match[2]
    if first == "":  # suffix: the last N bytes
        n = int(last)
        if n == 0 or size == 0:
            raise RangeError
        return max(size - n, 0), size - 1
    start = int(first)
    if last != "" and int(last) < start:
        return None  # an invalid range is ignored, not refused
    if start >= size:
        raise RangeError
    return start, size - 1 if last == "" else min(int(last), size - 1)


def build_label(manifest: list[dict]) -> str:
    """The menu row of the build the manifest describes."""
    by_name = {row["name"]: row for row in manifest}
    ramdisk = by_name.get("ramdisk")
    kernel = by_name.get("kernel-x86_64")
    if not ramdisk or not kernel:
        return "spike build"
    return (f"spike build  kernel {kernel['sha256'][:8]}  ramdisk {ramdisk['sha256'][:8]}  "
            f"{common.mib(ramdisk['size'])}")


def load_manifest(root: Path) -> list[dict]:
    try:
        return json.loads((root / "manifest.json").read_text())
    except (OSError, ValueError):
        return []


def served_file(root: Path, name: str) -> Path | None:
    """`root/name` if `name` is a plain file name of the root (no separators, no dot)."""
    if not name or name.startswith(".") or "/" in name or "\\" in name or "\0" in name:
        return None
    path = root / name
    return path if path.is_file() else None


def make_handler(root: Path, reg: str, timeout_ms: int, log):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        server_version = "lazyos-spike"

        def log_message(self, *_args) -> None:  # replaced by our own line per request
            pass

        def _finish(self, status: int, sent: int, started: float, note: str = "") -> None:
            seconds = time.monotonic() - started
            rate = f"{sent / seconds / 1e6:7.1f} MB/s" if sent > 100_000 and seconds > 0 else ""
            log(f"{time.strftime('%H:%M:%S')} {self.client_address[0]} {self.command} {self.path} "
                f"{status} {sent} B {seconds:.2f} s {rate} {note}".rstrip())

        def _text(self, body: str, started: float) -> None:
            data = body.encode()
            self.send_response(200)
            self.send_header("Content-Type", TEXT)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(data)
            self._finish(200, len(data), started)

        def _error(self, status: int, started: float, extra: dict | None = None) -> None:
            self.send_response(status)
            for key, value in (extra or {}).items():
                self.send_header(key, value)
            self.send_header("Content-Length", "0")
            self.end_headers()
            self._finish(status, 0, started)

        def _file(self, path: Path, started: float) -> None:
            size = path.stat().st_size
            try:
                span = parse_range(self.headers.get("Range"), size)
            except RangeError:
                self._error(416, started, {"Content-Range": f"bytes */{size}"})
                return
            first, last = span if span else (0, size - 1)
            length = last - first + 1 if size else 0
            self.send_response(206 if span else 200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Accept-Ranges", "bytes")
            self.send_header("Content-Length", str(length))
            if span:
                self.send_header("Content-Range", f"bytes {first}-{last}/{size}")
            self.end_headers()
            sent = 0
            if self.command != "HEAD":
                try:
                    with open(path, "rb") as f:
                        f.seek(first)
                        while sent < length:
                            chunk = f.read(min(CHUNK, length - sent))
                            if not chunk:
                                break
                            self.wfile.write(chunk)
                            sent += len(chunk)
                except (ConnectionError, TimeoutError):
                    self.close_connection = True
                    self._finish(206 if span else 200, sent, started, "(client went away)")
                    return
            self._finish(206 if span else 200, sent, started)

        def do_HEAD(self) -> None:
            self.do_GET()

        def do_GET(self) -> None:
            started = time.monotonic()
            route = self.path.split("?", 1)[0]
            if route == "/boot.ipxe":
                self._text(scripts.boot_menu(reg, build_label(load_manifest(root)), timeout_ms), started)
            elif route == "/build.ipxe":
                self._text(scripts.build_script(reg), started)
            elif route == "/sanboot.ipxe":
                self._text(scripts.sanboot_script(reg), started)
            elif route == "/":
                names = sorted(p.name for p in root.iterdir() if p.is_file())
                self._text("lazyos netboot spike\n" + "".join(f"/files/{n}\n" for n in names), started)
            elif route.startswith("/files/"):
                path = served_file(root, route[len("/files/"):])
                if path is None:
                    self._error(404, started)
                else:
                    self._file(path, started)
            else:
                self._error(404, started)

    return Handler


def serve(root: Path, bind: str, guest_host: str, timeout_ms: int, log=print) -> ThreadingHTTPServer:
    """A bound server (call `serve_forever`); `bind` port 0 picks a free one."""
    host, port = common.split_bind(bind)
    server = ThreadingHTTPServer((host, port), lambda *a, **k: None)
    reg = scripts.registry_url(guest_host, server.server_address[1])
    server.RequestHandlerClass = make_handler(root, reg, timeout_ms, log)
    server.registry_url = reg
    server.daemon_threads = True
    return server


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--root", default=str(common.DEFAULT_PAYLOAD))
    ap.add_argument("--bind", default=common.DEFAULT_BIND)
    ap.add_argument("--guest-host", default=common.SLIRP_HOST,
                    help="this server's address as the guest sees it (default %(default)s)")
    ap.add_argument("--timeout", type=int, default=scripts.DEFAULT_TIMEOUT_MS,
                    help="menu timeout in ms (default %(default)s)")
    ap.add_argument("--log", help="also append the request log to this file")
    args = ap.parse_args(argv)
    root = Path(args.root)
    if not root.is_dir():
        print(f"serve: {root} is not a directory (run extract_payload.py)", file=sys.stderr)
        return 2
    logfile = open(args.log, "a", buffering=1) if args.log else None

    def log(line: str) -> None:
        print(line, flush=True)
        if logfile:
            logfile.write(line + "\n")

    server = serve(root, args.bind, args.guest_host, args.timeout, log)
    print(f"serving {root} on {server.server_address[0]}:{server.server_address[1]}; "
          f"the guest uses {server.registry_url}", flush=True)
    missing = [n for n in (*common.PAYLOAD_FILES, common.LOADER_NAME) if not (root / n).is_file()]
    if missing:
        print(f"warning: missing from the root: {', '.join(missing)}", file=sys.stderr)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
