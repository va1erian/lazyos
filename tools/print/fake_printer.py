#!/usr/bin/env python3
"""A fake IPP Everywhere printer for the print harness (docs/printing-plan.md).

Standard library only. It speaks just enough IPP/2.0 over HTTP/1.1 for
LazyOS's print spooler (`printd`): Get-Printer-Attributes (a DeskJet 3700's
state and two ink levels), Create-Job (its attributes saved to
`<out>/job-<n>.json`), Send-Document (the document after the request saved to
`<out>/job-<n>.pwg`), Get-Job-Attributes (processing on the first ask,
completed after) and Cancel-Job. Print-Job (attributes and document at once)
is kept for older clients. Request bodies may be chunked, as clients send
them.

It also keeps what a real printer suffers from: requests whose body was cut
off (`Printer.truncated`) and jobs created but neither given their whole
document nor canceled (`Printer.open_jobs()`). `--slow` reads documents at a
printer's pace, so a client that quits soon after printing is still sending.

    python tools/print/fake_printer.py --port 8631 --out shots/print/jobs [--slow 20000]

The guest reaches it at 10.0.2.2:8631 through QEMU's user network.
"""

from __future__ import annotations

import argparse
import json
import struct
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

GROUP_OPERATION, GROUP_JOB, GROUP_END, GROUP_PRINTER = 1, 2, 3, 4
GET_PRINTER_ATTRIBUTES, PRINT_JOB, GET_JOB_ATTRIBUTES, CANCEL_JOB = 0x0B, 0x02, 0x09, 0x08
CREATE_JOB, SEND_DOCUMENT = 0x05, 0x06
JOB_PROCESSING, JOB_CANCELED, JOB_COMPLETED = 5, 7, 9


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


class Truncated(Exception):
    """The request's body ended before its last chunk."""


class Printer:
    """The fake printer's state: saved jobs and how often each was asked for."""

    def __init__(self, out: Path, slow: int = 0):
        self.out = out
        #: bytes per second a document is read at (0: as fast as it comes).
        self.slow = slow
        self.lock = threading.Lock()
        self.jobs = 0
        self.polls: dict[int, int] = {}
        self.log: list[str] = []
        self.truncated = 0
        self.created: set[int] = set()
        self.finished: set[int] = set()
        self.canceled: set[int] = set()

    def open_jobs(self) -> list[int]:
        """Jobs created and neither given their document nor canceled."""
        with self.lock:
            return sorted(self.created - self.finished - self.canceled)

    def _job(self, request_id: int, job_id: int, state: int) -> bytes:
        job = integer(0x21, "job-id", job_id) + integer(0x23, "job-state", state)
        return reply(request_id, [(GROUP_JOB, job)])

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
            if op in (PRINT_JOB, CREATE_JOB):
                self.jobs += 1
                job_id = self.jobs
                self.created.add(job_id)
                (self.out / f"job-{job_id}.json").write_text(json.dumps(attrs, indent=1))
                if op == PRINT_JOB:
                    (self.out / f"job-{job_id}.pwg").write_bytes(body[end:])
                    self.finished.add(job_id)
                return self._job(request_id, job_id, JOB_PROCESSING)
            if op == SEND_DOCUMENT:
                job_id = attrs.get("1.job-id", [0])[0]
                if job_id not in self.created:
                    return reply(request_id, [], status=0x0406)
                ticket_path = self.out / f"job-{job_id}.json"
                ticket = json.loads(ticket_path.read_text())
                ticket.update(attrs)
                ticket_path.write_text(json.dumps(ticket, indent=1))
                (self.out / f"job-{job_id}.pwg").write_bytes(body[end:])
                self.finished.add(job_id)
                return self._job(request_id, job_id, JOB_PROCESSING)
            if op == CANCEL_JOB:
                job_id = attrs.get("1.job-id", [0])[0]
                self.canceled.add(job_id)
                return self._job(request_id, job_id, JOB_CANCELED)
            if op == GET_JOB_ATTRIBUTES:
                job_id = attrs.get("1.job-id", [0])[0]
                self.polls[job_id] = self.polls.get(job_id, 0) + 1
                if job_id in self.canceled:
                    state = JOB_CANCELED
                else:
                    state = JOB_COMPLETED if self.polls[job_id] > 1 else JOB_PROCESSING
                return self._job(request_id, job_id, state)
            return reply(request_id, [], status=0x0501)


def read_exactly(handler: BaseHTTPRequestHandler, size: int, slow: int) -> bytes:
    """`size` bytes of the body, at `slow` bytes a second when set."""
    if not slow:
        data = handler.rfile.read(size)
    else:
        data = b""
        while len(data) < size:
            piece = handler.rfile.read(min(4096, size - len(data)))
            if not piece:
                break
            data += piece
            time.sleep(len(piece) / slow)
    if len(data) < size:
        raise Truncated
    return data


def read_body(handler: BaseHTTPRequestHandler, slow: int = 0) -> bytes:
    if "chunked" in handler.headers.get("Transfer-Encoding", "").lower():
        body = bytearray()
        while True:
            line = handler.rfile.readline()
            if not line.endswith(b"\n"):
                raise Truncated
            size = int(line.split(b";")[0].strip(), 16)
            if size == 0:
                handler.rfile.readline()
                return bytes(body)
            body += read_exactly(handler, size, slow)
            if not handler.rfile.readline().endswith(b"\n"):
                raise Truncated
    return read_exactly(handler, int(handler.headers.get("Content-Length", "0")), slow)


def serve(port: int, out: Path, host: str = "127.0.0.1",
          slow: int = 0) -> tuple[ThreadingHTTPServer, Printer]:
    """Starts the printer on a background thread."""
    out.mkdir(parents=True, exist_ok=True)
    printer = Printer(out, slow)

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def do_POST(self):  # noqa: N802 (http.server's naming)
            try:
                answer = printer.handle(read_body(self, printer.slow))
            except (Truncated, ConnectionError, TimeoutError):
                with printer.lock:
                    printer.truncated += 1
                    printer.log.append("truncated request")
                self.close_connection = True
                return
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
    parser.add_argument("--slow", type=int, default=0, metavar="BYTES_PER_S",
                        help="read documents at this pace, as a printer printing them")
    args = parser.parse_args()
    server, printer = serve(args.port, args.out, slow=args.slow)
    print(f"fake printer on ipp://127.0.0.1:{args.port}/ipp/print, jobs in {args.out}")
    try:
        threading.Event().wait()
    except KeyboardInterrupt:
        server.shutdown()
    print("\n".join(printer.log))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
