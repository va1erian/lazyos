#!/usr/bin/env python3
"""A fake IPP Everywhere printer for the print harness (docs/printing-plan.md).

Standard library only. It speaks just enough IPP/2.0 over HTTP/1.1 for
LazyWriter's print client: Get-Printer-Attributes (a DeskJet 3700's state and
two ink levels), Print-Job (the document after the request is saved to
`<out>/job-<n>.pwg`, the request's attributes to `<out>/job-<n>.json`) and
Get-Job-Attributes (processing on the first ask, completed after). Request
bodies may be chunked, as the client sends them.

    python tools/print/fake_printer.py --port 8631 --out shots/print/jobs

The guest reaches it at 10.0.2.2:8631 through QEMU's user network.
"""

from __future__ import annotations

import argparse
import json
import struct
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

GROUP_OPERATION, GROUP_JOB, GROUP_END, GROUP_PRINTER = 1, 2, 3, 4
GET_PRINTER_ATTRIBUTES, PRINT_JOB, GET_JOB_ATTRIBUTES, CANCEL_JOB = 0x0B, 0x02, 0x09, 0x08
JOB_PROCESSING, JOB_COMPLETED = 5, 9


def attribute(tag: int, name: str, value: bytes) -> bytes:
    raw = name.encode()
    return struct.pack(">BH", tag, len(raw)) + raw + struct.pack(">H", len(value)) + value


def integer(tag: int, name: str, n: int) -> bytes:
    return attribute(tag, name, struct.pack(">i", n))


def parse(body: bytes) -> tuple[int, int, dict[str, list], int]:
    """Operation, request id, the attributes as `group.name -> [values]`
    (strings decoded, integers as int), and where the document starts."""
    _version, op, request_id = struct.unpack(">HHI", body[:8])
    attrs: dict[str, list] = {}
    at, group, last, depth = 8, 0, "", 0
    while at < len(body):
        tag = body[at]
        at += 1
        if tag == GROUP_END:
            break
        if tag <= 0x0F:
            group = tag
            continue
        (name_len,) = struct.unpack(">H", body[at:at + 2])
        name = body[at + 2:at + 2 + name_len].decode("utf-8", "replace")
        at += 2 + name_len
        (value_len,) = struct.unpack(">H", body[at:at + 2])
        value = body[at + 2:at + 2 + value_len]
        at += 2 + value_len
        if tag == 0x34:
            depth += 1
        elif tag == 0x37:
            depth -= 1
        if depth or tag in (0x34, 0x37, 0x4A):
            continue
        key = f"{group}.{name or last}"
        last = name or last
        if tag in (0x21, 0x23) and len(value) == 4:
            decoded: object = struct.unpack(">i", value)[0]
        elif tag == 0x22:
            decoded = bool(value and value[0])
        else:
            decoded = value.decode("utf-8", "replace")
        attrs.setdefault(key, []).append(decoded)
    return op, request_id, attrs, at


def reply(request_id: int, groups: list[tuple[int, bytes]], status: int = 0) -> bytes:
    body = struct.pack(">BBHI", 2, 0, status, request_id)
    body += bytes([GROUP_OPERATION])
    body += attribute(0x47, "attributes-charset", b"utf-8")
    body += attribute(0x48, "attributes-natural-language", b"en")
    for tag, attrs in groups:
        body += bytes([tag]) + attrs
    return body + bytes([GROUP_END])


class Printer:
    """The fake printer's state: saved jobs and how often each was asked for."""

    def __init__(self, out: Path):
        self.out = out
        self.lock = threading.Lock()
        self.jobs = 0
        self.polls: dict[int, int] = {}
        self.log: list[str] = []

    def handle(self, body: bytes) -> bytes:
        op, request_id, attrs, end = parse(body)
        with self.lock:
            self.log.append(f"op=0x{op:04x} attrs={sorted(attrs)}")
            if op == GET_PRINTER_ATTRIBUTES:
                printer = (
                    attribute(0x41, "printer-make-and-model", b"HP DeskJet 3700 series (fake)")
                    + integer(0x23, "printer-state", 3)
                    + attribute(0x42, "marker-names", b"tri-color ink")
                    + attribute(0x42, "", b"black ink")
                    + integer(0x21, "marker-levels", 90)
                    + integer(0x21, "", 50)
                )
                return reply(request_id, [(GROUP_PRINTER, printer)])
            if op == PRINT_JOB:
                self.jobs += 1
                job_id = self.jobs
                (self.out / f"job-{job_id}.pwg").write_bytes(body[end:])
                (self.out / f"job-{job_id}.json").write_text(json.dumps(attrs, indent=1))
                job = integer(0x21, "job-id", job_id) + integer(0x23, "job-state", JOB_PROCESSING)
                return reply(request_id, [(GROUP_JOB, job)])
            if op in (GET_JOB_ATTRIBUTES, CANCEL_JOB):
                job_id = attrs.get("1.job-id", [0])[0]
                self.polls[job_id] = self.polls.get(job_id, 0) + 1
                state = JOB_COMPLETED if self.polls[job_id] > 1 else JOB_PROCESSING
                job = integer(0x21, "job-id", job_id) + integer(0x23, "job-state", state)
                return reply(request_id, [(GROUP_JOB, job)])
            return reply(request_id, [], status=0x0501)


def read_body(handler: BaseHTTPRequestHandler) -> bytes:
    if "chunked" in handler.headers.get("Transfer-Encoding", "").lower():
        body = bytearray()
        while True:
            size = int(handler.rfile.readline().split(b";")[0].strip(), 16)
            if size == 0:
                handler.rfile.readline()
                return bytes(body)
            body += handler.rfile.read(size)
            handler.rfile.readline()
    return handler.rfile.read(int(handler.headers.get("Content-Length", "0")))


def serve(port: int, out: Path, host: str = "127.0.0.1") -> tuple[ThreadingHTTPServer, Printer]:
    """Starts the printer on a background thread."""
    out.mkdir(parents=True, exist_ok=True)
    printer = Printer(out)

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def do_POST(self):  # noqa: N802 (http.server's naming)
            try:
                answer = printer.handle(read_body(self))
            except (ValueError, struct.error) as error:
                printer.log.append(f"bad request: {error}")
                self.send_error(400)
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/ipp")
            self.send_header("Content-Length", str(len(answer)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(answer)

        def log_message(self, *_args):
            pass

    server = ThreadingHTTPServer((host, port), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, printer


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--port", type=int, default=8631)
    parser.add_argument("--out", type=Path, default=Path("shots/print/jobs"))
    args = parser.parse_args()
    server, printer = serve(args.port, args.out)
    print(f"fake printer on ipp://127.0.0.1:{args.port}/ipp/print, jobs in {args.out}")
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        server.shutdown()
    print("\n".join(printer.log))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
