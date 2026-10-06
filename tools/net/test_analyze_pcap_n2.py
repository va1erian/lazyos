#!/usr/bin/env python3
"""Unit tests for the pcap analyzer's stage N2 checks (DHCP, IP sanity, ICMP
echo); `test_analyze_pcap.py` runs them too.
"""

from __future__ import annotations

import struct
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pcap  # noqa: E402
from pcap_fixtures import (  # noqa: E402
    GATEWAY_MAC, GUEST, GUEST_ADDR, analyze_n2, dhcp_exchange, dhcp_frame, echo_frame,
    ip_packet, ping_pair, reply, request, text,
)


class Dhcp(unittest.TestCase):
    def test_a_complete_exchange_passes(self):
        report = analyze_n2(dhcp_exchange(), dhcp=1)
        self.assertTrue(report.ok, text(report))
        self.assertIn("NET:PCAP:DHCP:PASS exchanges=1", text(report))

    def test_several_exchanges_are_counted(self):
        frames = dhcp_exchange(1) + dhcp_exchange(2) + dhcp_exchange(3)
        self.assertTrue(analyze_n2(frames, dhcp=3).ok)
        report = analyze_n2(frames, dhcp=4)
        self.assertFalse(report.ok)
        self.assertIn("3 complete DHCP exchange(s), 4 required", text(report))

    def test_no_dhcp_at_all_fails(self):
        report = analyze_n2([request(), reply()], dhcp=1)
        self.assertFalse(report.ok)
        self.assertIn("no DHCP message", text(report))

    def test_each_missing_message_fails(self):
        for drop, name in enumerate(["DISCOVER", "OFFER", "REQUEST", "ACK"]):
            frames = [f for i, f in enumerate(dhcp_exchange()) if i != drop]
            report = analyze_n2(frames, dhcp=1)
            self.assertFalse(report.ok, name)
            self.assertIn("0 complete DHCP exchange(s)", text(report), name)

    def test_the_wrong_order_fails(self):
        d, o, r, a = dhcp_exchange()
        for frames in ([d, r, o, a], [o, d, r, a], [a, r, o, d]):
            self.assertFalse(analyze_n2(frames, dhcp=1).ok)

    def test_messages_with_different_transaction_ids_do_not_combine(self):
        frames = [dhcp_frame(1, 1), dhcp_frame(2, 2), dhcp_frame(3, 1), dhcp_frame(5, 2)]
        self.assertFalse(analyze_n2(frames, dhcp=1).ok)

    def test_an_ack_that_grants_another_address_fails(self):
        d, o, r, _ = dhcp_exchange()
        ack = dhcp_frame(5, yiaddr=bytes([10, 0, 2, 99]))
        report = analyze_n2([d, o, r, ack], dhcp=1)
        self.assertFalse(report.ok)
        self.assertIn("the ACK grants 10.0.2.99 but the OFFER named 10.0.2.15", text(report))

    def test_an_offer_of_the_unspecified_address_fails(self):
        d, _, r, _ = dhcp_exchange()
        zero = bytes(4)
        report = analyze_n2([d, dhcp_frame(2, yiaddr=zero), r, dhcp_frame(5, yiaddr=zero)], dhcp=1)
        self.assertFalse(report.ok)

    def test_a_nak_fails(self):
        d, o, r, _ = dhcp_exchange()
        report = analyze_n2([d, o, r, dhcp_frame(6)], dhcp=1)
        self.assertFalse(report.ok)
        self.assertIn("DHCPNAK", text(report))

    def test_a_client_with_another_hardware_address_fails(self):
        frames = dhcp_exchange()
        frames[0] = dhcp_frame(1, chaddr=bytes.fromhex("525400aabbcc"))
        report = analyze_n2(frames, dhcp=1)
        self.assertFalse(report.ok)
        self.assertIn("not the guest's", text(report))

    def test_a_truncated_dhcp_frame_does_not_count(self):
        frames = dhcp_exchange()
        frames[3] = frames[3][:200]
        self.assertFalse(analyze_n2(frames, dhcp=1).ok)


class Echo(unittest.TestCase):
    def test_complete_pairs_pass(self):
        frames = ping_pair(seq=1) + ping_pair(seq=2, data=b"x" * 56) + ping_pair(seq=3, data=b"")
        report = analyze_n2(frames, pings=3)
        self.assertTrue(report.ok, text(report))
        self.assertIn("NET:PCAP:PING:PASS pairs=3", text(report))
        self.assertIn("NET:PCAP:IP:PASS", text(report))

    def test_a_reply_addressed_to_another_ip_fails(self):
        # Right MAC, identifier, sequence, payload and checksums, wrong IPv4 destination.
        frames = [
            echo_frame(8, 7, 1, b"abcdefgh", from_guest=True),
            echo_frame(0, 7, 1, b"abcdefgh", from_guest=False, reply_to=bytes([10, 0, 2, 99])),
        ]
        report = analyze_n2(frames, pings=1)
        self.assertFalse(report.ok)
        self.assertIn("has no reply after it", text(report))

    def test_a_missing_reply_fails(self):
        report = analyze_n2(ping_pair()[:1], pings=1)
        self.assertFalse(report.ok)
        self.assertIn("no ICMP echo reply", text(report))

    def test_a_missing_request_fails(self):
        report = analyze_n2(ping_pair()[1:], pings=1)
        self.assertFalse(report.ok)
        self.assertIn("no ICMP echo request", text(report))

    def test_the_wrong_order_fails(self):
        report = analyze_n2(list(reversed(ping_pair())), pings=1)
        self.assertFalse(report.ok)
        self.assertIn("comes before", text(report))

    def test_a_wrong_payload_fails(self):
        request_frame = echo_frame(8, 7, 1, b"abcdefgh", from_guest=True)
        reply_frame = echo_frame(0, 7, 1, b"abcdefgX", from_guest=False)
        report = analyze_n2([request_frame, reply_frame], pings=1)
        self.assertFalse(report.ok)
        self.assertIn("payload differs", text(report))

    def test_a_reply_with_another_identifier_or_sequence_does_not_answer(self):
        request_frame = echo_frame(8, 7, 1, b"data", from_guest=True)
        for ident, seq in ((8, 1), (7, 2)):
            report = analyze_n2([request_frame, echo_frame(0, ident, seq, b"data", from_guest=False)], pings=1)
            self.assertFalse(report.ok, (ident, seq))
            self.assertIn("has no reply after it", text(report))

    def test_a_bad_reply_checksum_fails(self):
        frames = [echo_frame(8, 7, 1, b"data", from_guest=True), echo_frame(0, 7, 1, b"data", from_guest=False, bad_checksum=True)]
        report = analyze_n2(frames, pings=1)
        self.assertFalse(report.ok)

    def test_a_bad_request_checksum_fails_the_ip_check(self):
        for kwargs, what in (({"bad_checksum": True}, "ICMP checksum"), ({"bad_ip": True}, "IPv4 header checksum")):
            frames = [echo_frame(8, 7, 1, b"data", from_guest=True, **kwargs), echo_frame(0, 7, 1, b"data", from_guest=False)]
            report = analyze_n2(frames, pings=1)
            self.assertFalse(report.ok, what)
            self.assertIn(what, text(report))

    def test_each_reply_answers_one_request(self):
        one = ping_pair()
        frames = [one[0], one[0], one[1]]
        report = analyze_n2(frames, pings=2)
        self.assertFalse(report.ok)

    def test_the_required_number_of_pairs(self):
        frames = ping_pair(seq=1) + ping_pair(seq=2)
        self.assertTrue(analyze_n2(frames, pings=2).ok)
        report = analyze_n2(frames, pings=3)
        self.assertFalse(report.ok)
        self.assertIn("2 complete echo exchange(s) with the gateway, 3 required", text(report))

    def test_an_echo_request_to_an_invalid_destination_reaching_the_wire_fails(self):
        for dst in ([127, 0, 0, 1], [0, 0, 0, 0], [224, 0, 0, 1], [255, 255, 255, 255]):
            frames = ping_pair() + [echo_frame(8, 7, 9, b"x", from_guest=True, dst=bytes(dst))]
            report = analyze_n2(frames, pings=1)
            self.assertFalse(report.ok, dst)
            self.assertIn("reached the wire", text(report))

    def test_replies_from_another_host_do_not_count(self):
        request_frame = echo_frame(8, 7, 1, b"data", from_guest=True)
        stranger = bytes([10, 0, 2, 99])
        icmp = struct.pack(">BBHHH", 0, 0, 0, 7, 1) + b"data"
        icmp = icmp[:2] + struct.pack(">H", pcap.ipv4_checksum(icmp)) + icmp[4:]
        forged = GUEST + GATEWAY_MAC + b"\x08\x00" + ip_packet(stranger, GUEST_ADDR, 1, icmp)
        self.assertFalse(analyze_n2([request_frame, forged], pings=1).ok)


if __name__ == "__main__":
    unittest.main()
