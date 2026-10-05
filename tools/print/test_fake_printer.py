#!/usr/bin/env python3
"""The fake printer keeps a Create-Job + Send-Document job, honours
Cancel-Job, and notices what a real printer suffers from: a request cut off
mid-body and a job created but never finished (tools/print/fake_printer.py).

    python tools/print/test_fake_printer.py
"""

from __future__ import annotations

import socket
import struct
import sys
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import fake_printer as fp  # noqa: E402


def request(op: int, request_id: int, job_id: int | None = None) -> bytes:
    body = struct.pack(">BBHI", 2, 0, op, request_id) + bytes([fp.GROUP_OPERATION])
    body += fp.attribute(0x47, "attributes-charset", b"utf-8")
    body += fp.attribute(0x48, "attributes-natural-language", b"en")
    body += fp.attribute(0x45, "printer-uri", b"ipp://127.0.0.1/ipp/print")
    if job_id is not None:
        body += fp.integer(0x21, "job-id", job_id)
    return body + bytes([fp.GROUP_END])


def post(port: int, body: bytes, cut: int | None = None) -> bytes:
    """Sends `body` chunked; with `cut`, hangs up after that many bytes."""
    head = (b"POST /ipp/print HTTP/1.1\r\nHost: x\r\nContent-Type: application/ipp\r\n"
            b"Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n")
    wire = head + b"%x\r\n" % len(body) + body + b"\r\n0\r\n\r\n"
    with socket.create_connection(("127.0.0.1", port)) as sock:
        if cut is not None:
            sock.sendall(wire[:len(head) + cut])
            return b""
        sock.sendall(wire)
        answer = b""
        while chunk := sock.recv(4096):
            answer += chunk
        return answer


def job_id(answer: bytes) -> int:
    body = answer.split(b"\r\n\r\n", 1)[1]
    _op, _rid, attrs, _end = fp.parse(body)
    return attrs["2.job-id"][0]


class FakePrinterTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.server, self.printer = fp.serve(0, Path(self.dir.name))
        self.port = self.server.server_address[1]

    def tearDown(self):
        self.server.shutdown()
        self.dir.cleanup()

    def test_create_then_send_keeps_the_document(self):
        job = job_id(post(self.port, request(fp.CREATE_JOB, 1)))
        self.assertEqual(self.printer.open_jobs(), [job])
        post(self.port, request(fp.SEND_DOCUMENT, 2, job) + b"RaS2 page")
        self.assertEqual(self.printer.open_jobs(), [])
        self.assertEqual((Path(self.dir.name) / f"job-{job}.pwg").read_bytes(), b"RaS2 page")
        self.assertEqual(self.printer.truncated, 0)

    def test_a_canceled_job_is_not_left_open(self):
        job = job_id(post(self.port, request(fp.CREATE_JOB, 1)))
        post(self.port, request(fp.CANCEL_JOB, 2, job))
        self.assertEqual(self.printer.open_jobs(), [])

    def test_a_cut_off_request_is_counted(self):
        job = job_id(post(self.port, request(fp.CREATE_JOB, 1)))
        post(self.port, request(fp.SEND_DOCUMENT, 2, job) + b"RaS2" * 100, cut=60)
        deadline = time.time() + 5
        while not self.printer.truncated and time.time() < deadline:
            time.sleep(0.02)
        self.assertEqual(self.printer.truncated, 1)
        self.assertEqual(self.printer.open_jobs(), [job])


if __name__ == "__main__":
    unittest.main()
