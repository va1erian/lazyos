#!/usr/bin/env python3
"""A tiny HTTP server on the host for `linuxapps_net.json`.

The guest reaches the host's loopback as `10.0.2.2` through QEMU's
user-mode network (as `tools/net/run.py`'s echo servers do), so this listens
on 127.0.0.1 and serves two fixed documents:

* ``/hello.txt``: ``HELLO-FROM-HOST``;
* ``/data.json``: a small JSON object that ``jq`` reads back.

Every request is logged to stdout, which is the server-side evidence that
the guest's BusyBox ``wget`` really crossed the wire.

    python tools/linuxapps/hostserver.py [--port 47790]
"""

from __future__ import annotations

import argparse
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

DOCUMENTS = {
    "/hello.txt": (b"HELLO-FROM-HOST\n", "text/plain"),
    "/data.json": (
        json.dumps({"items": [{"n": "a"}, {"n": "b"}, {"n": "c"}], "from": "host"}).encode(),
        "application/json",
    ),
}


class Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:  # noqa: N802 (http.server's naming)
        body, kind = DOCUMENTS.get(self.path, (b"not found\n", "text/plain"))
        self.send_response(200 if self.path in DOCUMENTS else 404)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt: str, *args) -> None:
        print(f"HOSTSERVER:{self.command} {self.path} {args[1] if len(args) > 1 else ''}",
              flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--port", type=int, default=47790)
    args = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"HOSTSERVER:LISTEN 127.0.0.1:{args.port}", flush=True)
    server.serve_forever()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
