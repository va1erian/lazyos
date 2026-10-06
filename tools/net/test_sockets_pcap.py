#!/usr/bin/env python3
"""Unit tests for the socket evidence checker: it must fail when it should.

Synthetic captures of a guest talking to a gateway echo server (TCP, UDP, DNS)
are built frame by frame; each check is shown to pass on a good capture and to
fail on every way the wire can be wrong: a flipped payload byte, a lost
segment, a missing FIN, a wrong checksum, a handshake that never completed, a
datagram that was not echoed, a query for the wrong name, and a host server
that saw different bytes than the capture shows.
"""

from __future__ import annotations

import struct
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import hostpeers  # noqa: E402
import sockets_pcap as sp  # noqa: E402
from sockets_fixtures import (  # noqa: E402
    DNS_IP, GATEWAY_IP, GUEST, GUEST_IP, PORT, check_flows, dns_query, echo_flow, frames_of,
    tcp_frame, udp_frame,
)
# The FTP judge and the host peers have their own file; this script runs them too.
from test_sockets_ftp import Ftp, Peers  # noqa: E402,F401


class TcpFlows(unittest.TestCase):
    PAYLOAD = hostpeers.pattern(4000)

    def test_a_good_echo_passes(self):
        raw = echo_flow(50000, self.PAYLOAD) + echo_flow(50001, b"hello\n")
        count, problems = check_flows(raw, [self.PAYLOAD, b"hello\n"], 2)
        self.assertEqual((count, problems), (2, []))

    def test_a_flipped_echo_byte_fails(self):
        _, problems = check_flows(echo_flow(50000, self.PAYLOAD, flip=2500), [self.PAYLOAD])
        self.assertTrue(any("differ" in p for p in problems), problems)

    def test_a_short_echo_fails(self):
        _, problems = check_flows(echo_flow(50000, self.PAYLOAD, drop_echo_tail=10), [self.PAYLOAD])
        self.assertTrue(any("lengths differ" in p for p in problems), problems)

    def test_a_missing_guest_fin_fails(self):
        _, problems = check_flows(echo_flow(50000, self.PAYLOAD, fin_guest=False), [self.PAYLOAD])
        self.assertTrue(any("FIN" in p for p in problems), problems)

    def test_a_missing_gateway_fin_fails(self):
        _, problems = check_flows(echo_flow(50000, self.PAYLOAD, fin_gateway=False), [self.PAYLOAD])
        self.assertTrue(any("FIN" in p for p in problems), problems)

    def test_no_handshake_fails(self):
        _, problems = check_flows(echo_flow(50000, self.PAYLOAD, handshake=False), [self.PAYLOAD])
        self.assertTrue(any("handshake" in p for p in problems), problems)

    def test_too_few_flows_fails(self):
        _, problems = check_flows(echo_flow(50000, b"x"), [b"x"], min_flows=2)
        self.assertTrue(any("2 required" in p for p in problems), problems)

    def test_a_lost_segment_is_a_gap(self):
        raw = echo_flow(50000, self.PAYLOAD)
        data_frames = [i for i, f in enumerate(raw) if len(f) > 1000 and f[6:12] == GUEST]
        del raw[data_frames[1]]
        _, problems = check_flows(raw, [self.PAYLOAD])
        self.assertTrue(any("gap" in p for p in problems), problems)

    def test_a_retransmission_is_fine(self):
        raw = echo_flow(50000, self.PAYLOAD)
        first = next(i for i, f in enumerate(raw) if len(f) > 1000 and f[6:12] == GUEST)
        raw.insert(first + 1, raw[first])
        count, problems = check_flows(raw, [self.PAYLOAD])
        self.assertEqual((count, problems), (1, []))

    def test_the_host_server_seeing_other_bytes_fails(self):
        _, problems = check_flows(echo_flow(50000, self.PAYLOAD), [self.PAYLOAD[:-1] + b"?"])
        self.assertTrue(any("do not match what the host server" in p for p in problems), problems)

    def test_the_host_server_seeing_nothing_fails(self):
        _, problems = check_flows(echo_flow(50000, self.PAYLOAD), [])
        self.assertTrue(any("do not match" in p for p in problems), problems)

    def test_a_reset_flow_fails(self):
        raw = echo_flow(50000, b"x")
        raw.append(tcp_frame(False, PORT, 50000, 9999, 0, sp.RST))
        _, problems = check_flows(raw, [b"x"])
        self.assertTrue(any("reset" in p for p in problems), problems)

    def test_flows_to_another_port_are_not_counted(self):
        _, problems = check_flows(echo_flow(50000, b"x", dport=PORT + 1), [], min_flows=1)
        self.assertTrue(any("0 TCP flows" in p for p in problems), problems)

    def test_a_reused_port_is_a_new_flow(self):
        raw = echo_flow(50000, b"one") + echo_flow(50000, b"two")
        count, problems = check_flows(raw, [b"one", b"two"], 2)
        self.assertEqual((count, problems), (2, []))


class Checksums(unittest.TestCase):
    def test_good_segments_pass(self):
        self.assertEqual(sp.check_checksums(frames_of(echo_flow(50000, b"abc")), GUEST), [])

    def test_a_wrong_tcp_checksum_fails(self):
        raw = [tcp_frame(True, 50000, PORT, 1, 0, sp.SYN, break_checksum=True)]
        self.assertTrue(sp.check_checksums(frames_of(raw), GUEST))

    def test_a_wrong_udp_checksum_fails(self):
        frame = bytearray(udp_frame(True, 50000, 47772, b"abc"))
        frame[14 + 20 + 6] ^= 0x55
        self.assertTrue(sp.check_checksums(frames_of([bytes(frame)]), GUEST))

    def test_the_gateways_frames_are_not_judged(self):
        raw = [tcp_frame(False, PORT, 50000, 1, 0, sp.SYN | sp.ACK, break_checksum=True)]
        self.assertEqual(sp.check_checksums(frames_of(raw), GUEST), [])


class Refusals(unittest.TestCase):
    def syn_rst(self, sport):
        return [tcp_frame(True, sport, 47999, 1, 0, sp.SYN), tcp_frame(False, 47999, sport, 0, 2, sp.RST | sp.ACK)]

    def test_refused_connections_are_counted(self):
        raw = self.syn_rst(50000) + self.syn_rst(50001)
        count, problems = sp.check_refused(frames_of(raw), GUEST_IP, GATEWAY_IP, 47999, 2)
        self.assertEqual((count, problems), (2, []))

    def test_silence_is_an_acceptable_answer_but_still_an_attempt(self):
        raw = [tcp_frame(True, 50000, 47999, 1, 0, sp.SYN)]
        count, problems = sp.check_refused(frames_of(raw), GUEST_IP, GATEWAY_IP, 47999, 1)
        self.assertEqual((count, problems), (0, []))

    def test_too_few_attempts_fail(self):
        count, problems = sp.check_refused(frames_of([]), GUEST_IP, GATEWAY_IP, 47999, 1)
        self.assertEqual(count, 0)
        self.assertTrue(any("1 required" in p for p in problems), problems)

    def test_an_established_connection_to_the_closed_port_fails(self):
        raw = echo_flow(50000, b"x", dport=47999)
        _, problems = sp.check_refused(frames_of(raw), GUEST_IP, GATEWAY_IP, 47999, 1)
        self.assertTrue(any("was established" in p for p in problems), problems)


class UdpEcho(unittest.TestCase):
    def pair(self, sport, payload, reply=None):
        return [udp_frame(True, sport, 47772, payload), udp_frame(False, 47772, sport, payload if reply is None else reply)]

    def test_good_pairs_pass(self):
        raw = self.pair(50000, b"a") + self.pair(50001, b"bb")
        count, problems = sp.check_udp_echo(frames_of(raw), GUEST_IP, GATEWAY_IP, 47772, 2, [b"a", b"bb"])
        self.assertEqual((count, problems), (2, []))

    def test_a_different_reply_fails(self):
        raw = self.pair(50000, b"a", reply=b"b")
        _, problems = sp.check_udp_echo(frames_of(raw), GUEST_IP, GATEWAY_IP, 47772, 1, [b"a"])
        self.assertTrue(any("not echoed" in p for p in problems), problems)

    def test_no_reply_fails(self):
        raw = [udp_frame(True, 50000, 47772, b"a")]
        _, problems = sp.check_udp_echo(frames_of(raw), GUEST_IP, GATEWAY_IP, 47772, 1, [b"a"])
        self.assertTrue(problems)

    def test_the_server_seeing_other_datagrams_fails(self):
        raw = self.pair(50000, b"a")
        _, problems = sp.check_udp_echo(frames_of(raw), GUEST_IP, GATEWAY_IP, 47772, 1, [b"z"])
        self.assertTrue(any("do not match" in p for p in problems), problems)


class Dns(unittest.TestCase):
    def query_frame(self, name="localhost", sport=50053, **kw):
        return udp_frame(True, sport, 53, dns_query(name, **kw), dst_ip=DNS_IP)

    def test_a_query_without_an_answer_is_reported_not_failed(self):
        detail, problems = sp.check_dns(frames_of([self.query_frame()]), GUEST_IP, "localhost")
        self.assertEqual(problems, [])
        self.assertIn("no answer", detail)

    def test_an_answer_is_reported(self):
        answer = bytearray(dns_query("localhost"))
        answer[2:4] = struct.pack(">H", 0x8183)
        raw = [self.query_frame(), udp_frame(False, 53, 50053, bytes(answer), src_ip=DNS_IP)]
        detail, problems = sp.check_dns(frames_of(raw), GUEST_IP, "localhost")
        self.assertEqual(problems, [])
        self.assertIn("rcode=3", detail)

    def test_no_query_fails(self):
        _, problems = sp.check_dns(frames_of([]), GUEST_IP, "localhost")
        self.assertTrue(problems)

    def test_a_query_for_another_name_fails(self):
        _, problems = sp.check_dns(frames_of([self.query_frame("example.test")]), GUEST_IP, "localhost")
        self.assertTrue(problems)

    def test_a_query_of_another_type_fails(self):
        _, problems = sp.check_dns(frames_of([self.query_frame(qtype=28)]), GUEST_IP, "localhost")
        self.assertTrue(any("not a well-formed A" in p for p in problems), problems)

    def test_the_name_parser_refuses_garbage(self):
        for message in (b"", b"\0" * 12, dns_query("a.b")[:15], b"\x12\x34\x01\x00\x00\x01" + b"\0" * 6 + b"\xff" + b"x" * 10):
            self.assertIsNone(sp.dns_name(message), message)


class Inbound(unittest.TestCase):
    PAYLOAD = hostpeers.pattern(3000)

    def flow(self, incoming, outgoing, **kw):
        """The harness (as the gateway) connecting to the guest's port 47773."""
        g, h = 7000, 9000
        raw = [tcp_frame(False, 40000, 47773, g, 0, sp.SYN),
               tcp_frame(True, 47773, 40000, h, g + 1, sp.SYN | sp.ACK),
               tcp_frame(False, 40000, 47773, g + 1, h + 1, sp.ACK)]
        gseq, hseq = g + 1, h + 1
        for at in range(0, max(len(incoming), len(outgoing)), 1400):
            part = incoming[at:at + 1400]
            if part:
                raw.append(tcp_frame(False, 40000, 47773, gseq, hseq, sp.ACK, part))
                gseq += len(part)
            back = outgoing[at:at + 1400]
            if back:
                raw.append(tcp_frame(True, 47773, 40000, hseq, gseq, sp.ACK, back))
                hseq += len(back)
        return raw

    def check(self, raw):
        return sp.check_inbound_flow(frames_of(raw), GUEST_IP, GATEWAY_IP, 47773, self.PAYLOAD)

    def test_a_good_inbound_flow_passes(self):
        count, problems = self.check(self.flow(self.PAYLOAD, self.PAYLOAD))
        self.assertEqual((count, problems), (len(self.PAYLOAD), []))

    def test_an_echo_that_differs_fails(self):
        wrong = self.PAYLOAD[:-1] + b"!"
        _, problems = self.check(self.flow(self.PAYLOAD, wrong))
        self.assertTrue(any("echoed" in p for p in problems), problems)

    def test_a_short_receive_fails(self):
        _, problems = self.check(self.flow(self.PAYLOAD[:-5], self.PAYLOAD))
        self.assertTrue(any("received" in p for p in problems), problems)

    def test_no_connection_fails(self):
        _, problems = self.check([])
        self.assertTrue(any("no connection" in p for p in problems), problems)


if __name__ == "__main__":
    unittest.main()
