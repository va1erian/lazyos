#!/usr/bin/env python3
"""Unit tests for the TLS capture judge: it must fail when it should.

A real ClientHello (Python's own `ssl`, through a memory BIO) is put on a
synthetic connection from the guest to a TLS port; the judge passes it, and
fails a wrong SNI, a missing ALPN, a flow that does not start with TLS,
plaintext HTTP, a secret in the clear and too few connections.
"""

from __future__ import annotations

import ssl
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pcap  # noqa: E402
import sockets_pcap as sp  # noqa: E402
import tls_pcap  # noqa: E402
from test_sockets_pcap import GATEWAY_IP, GUEST_IP, tcp_frame  # noqa: E402

PORT = 47790
SECRET = b"the page body only TLS may carry"


def client_hello(name: str = "tls.test", alpn: list[str] | None = None) -> bytes:
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.check_hostname = False
    context.verify_mode = ssl.CERT_NONE
    if alpn is None:
        alpn = ["http/1.1"]
    if alpn:
        context.set_alpn_protocols(alpn)
    incoming, outgoing = ssl.MemoryBIO(), ssl.MemoryBIO()
    tls = context.wrap_bio(incoming, outgoing, server_hostname=name)
    try:
        tls.do_handshake()
    except ssl.SSLWantReadError:
        pass
    return outgoing.read()


def flow(client: bytes, server: bytes = b"\x17\x03\x03\x00\x05abcde", sport: int = 50000,
         port: int = PORT) -> list[bytes]:
    g, h = 1000, 5000
    return [tcp_frame(True, sport, port, g, 0, sp.SYN),
            tcp_frame(False, port, sport, h, g + 1, sp.SYN | sp.ACK),
            tcp_frame(True, sport, port, g + 1, h + 1, sp.ACK, client),
            tcp_frame(False, port, sport, h + 1, g + 1 + len(client), sp.ACK, server),
            tcp_frame(True, sport, port, g + 1 + len(client), h + 1 + len(server), sp.FIN | sp.ACK),
            tcp_frame(False, port, sport, h + 1 + len(server), g + 2 + len(client), sp.FIN | sp.ACK)]


def judge(raw: list[bytes], minimum: int = 1) -> tuple[int, list[str]]:
    frames = pcap.read_pcap(pcap.write_pcap(raw))
    return tls_pcap.check_tls_flows(frames, GUEST_IP, GATEWAY_IP, {PORT: "tls.test"},
                                    {PORT: minimum}, [SECRET])


class ClientHelloTests(unittest.TestCase):
    def test_a_real_hello_is_parsed(self) -> None:
        hello = tls_pcap.parse_client_hello(client_hello("rsa.tls.test"))
        self.assertIsNotNone(hello)
        self.assertEqual(hello.sni, "rsa.tls.test")
        self.assertEqual(hello.alpn, ["http/1.1"])
        self.assertTrue(hello.suites)

    def test_garbage_and_truncation_are_not_a_hello(self) -> None:
        good = client_hello()
        for bad in (b"", b"GET / HTTP/1.1\r\n\r\n", good[:40], b"\x16\x03\x01\x00\x04\x02\x00\x00\x00"):
            self.assertIsNone(tls_pcap.parse_client_hello(bad), bad[:12])


class FlowTests(unittest.TestCase):
    def test_a_good_connection_passes(self) -> None:
        count, problems = judge(flow(client_hello()))
        self.assertEqual((count, problems), (1, []))

    def test_the_wrong_name_fails(self) -> None:
        _, problems = judge(flow(client_hello("other.test")))
        self.assertTrue(any("SNI" in p for p in problems), problems)

    def test_no_alpn_fails(self) -> None:
        _, problems = judge(flow(client_hello(alpn=[])))
        self.assertTrue(any("ALPN" in p for p in problems), problems)

    def test_plaintext_fails(self) -> None:
        _, problems = judge(flow(b"GET / HTTP/1.1\r\nHost: tls.test\r\n\r\n"))
        self.assertTrue(any("ClientHello" in p for p in problems), problems)
        _, problems = judge(flow(client_hello(), server=b"HTTP/1.1 200 OK\r\n\r\n"))
        self.assertTrue(any("plaintext" in p for p in problems), problems)

    def test_a_secret_in_the_clear_fails(self) -> None:
        _, problems = judge(flow(client_hello(), server=b"\x17\x03\x03" + SECRET))
        self.assertTrue(any("secret" in p for p in problems), problems)

    def test_too_few_connections_fail(self) -> None:
        _, problems = judge(flow(client_hello()), minimum=2)
        self.assertTrue(any("expected at least 2" in p for p in problems), problems)

    def test_other_ports_are_not_judged(self) -> None:
        count, problems = judge(flow(b"plain", port=47771) + flow(client_hello(), sport=50001))
        self.assertEqual((count, problems), (1, []))


if __name__ == "__main__":
    unittest.main()
