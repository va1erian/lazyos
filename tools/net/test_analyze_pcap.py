#!/usr/bin/env python3
"""Unit tests for the pcap analyzer: the harness must fail when it should.

Every check is shown to pass on a good capture and to fail on each way the
capture can be wrong: a missing reply, a wrong payload, the wrong order, a
truncated capture, an empty file, frames outside the length policy, and (stage
N2) a DHCP exchange with a message missing, out of order, with mixed transaction
ids or a different granted address, and an ICMP echo exchange with a missing or
forged reply, a wrong payload, a bad checksum or a request to an address that
cannot be a host.
"""

from __future__ import annotations

import struct
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import analyze_pcap as ap  # noqa: E402
import pcap  # noqa: E402
from pcap_fixtures import (  # noqa: E402
    GATEWAY_IP, GATEWAY_MAC, GUEST, GUEST_IP, analyze, arp, probe, reply, request, text,
)
# Stage N2's checks live in their own file; this script runs them too.
from test_analyze_pcap_n2 import Dhcp, Echo  # noqa: E402,F401


class Reader(unittest.TestCase):
    def test_round_trip(self):
        frames = [request(), reply(), probe(14), probe(1514)]
        parsed = pcap.read_pcap(pcap.write_pcap(frames))
        self.assertEqual([f.data for f in parsed], frames)
        self.assertEqual([f.index for f in parsed], [0, 1, 2, 3])

    def test_an_empty_file_is_an_error_not_zero_packets(self):
        with self.assertRaisesRegex(pcap.PcapError, "empty"):
            pcap.read_pcap(b"")

    def test_a_header_only_capture_is_valid_but_has_no_frames(self):
        self.assertEqual(pcap.read_pcap(pcap.write_pcap([])), [])

    def test_a_truncated_capture_is_an_error(self):
        good = pcap.write_pcap([request(), reply()])
        for cut in (5, 23, len(good) - 1, len(good) - 30, 24 + 10):
            with self.assertRaises(pcap.PcapError, msg=f"cut at {cut}"):
                pcap.read_pcap(good[:cut])

    def test_bad_magic_and_link_type(self):
        good = bytearray(pcap.write_pcap([request()]))
        bad_magic = bytes([0]) + bytes(good[1:])
        with self.assertRaisesRegex(pcap.PcapError, "magic"):
            pcap.read_pcap(bad_magic)
        good[20:24] = struct.pack("<I", 113)
        with self.assertRaisesRegex(pcap.PcapError, "link type"):
            pcap.read_pcap(bytes(good))

    def test_an_absurd_record_length_is_refused_not_allocated(self):
        header = pcap.write_pcap([])
        record = struct.pack("<IIII", 1, 0, 0x7FFFFFFF, 0x7FFFFFFF)
        with self.assertRaisesRegex(pcap.PcapError, "claims"):
            pcap.read_pcap(header + record)

    def test_nanosecond_captures_are_read(self):
        data = bytearray(pcap.write_pcap([request()]))
        data[0:4] = struct.pack("<I", pcap.MAGIC_LE_NS)
        self.assertEqual(len(pcap.read_pcap(bytes(data))), 1)


class Arp(unittest.TestCase):
    def test_a_complete_exchange_passes(self):
        report = analyze([request(), reply()])
        self.assertTrue(report.ok, text(report))
        self.assertIn("NET:PCAP:ARP:PASS pairs=1", text(report))

    def test_a_missing_reply_fails(self):
        report = analyze([request()])
        self.assertFalse(report.ok)
        self.assertIn("no ARP reply", text(report))

    def test_a_missing_request_fails(self):
        report = analyze([reply()])
        self.assertFalse(report.ok)
        self.assertIn("no ARP request", text(report))

    def test_the_wrong_order_fails(self):
        report = analyze([reply(), request()])
        self.assertFalse(report.ok)
        self.assertIn("comes before", text(report))
        self.assertIn("has no reply after it", text(report))

    def test_a_reply_for_another_address_fails(self):
        report = analyze([request(), reply(sender_ip=bytes([10, 0, 2, 99]))])
        self.assertFalse(report.ok)
        self.assertIn("no ARP reply", text(report))

    def test_a_request_for_another_address_fails(self):
        report = analyze([request(target=bytes([10, 0, 2, 3])), reply()])
        self.assertFalse(report.ok)

    def test_a_reply_not_addressed_to_the_guest_fails(self):
        stranger = bytes.fromhex("525400aabbcc")
        bad = arp(2, stranger, GATEWAY_MAC, GATEWAY_MAC, GATEWAY_IP, stranger, GUEST_IP)
        report = analyze([request(), bad])
        self.assertFalse(report.ok)
        self.assertIn("not addressed to the guest", text(report))

    def test_a_reply_with_inconsistent_sender_fields_fails(self):
        liar = arp(2, GUEST, bytes.fromhex("525500000999"), GATEWAY_MAC, GATEWAY_IP, GUEST, GUEST_IP)
        report = analyze([request(), liar])
        self.assertFalse(report.ok)
        self.assertIn("differs from its ARP sender", text(report))

    def test_a_truncated_reply_does_not_count(self):
        report = analyze([request(), reply()[:41]])
        self.assertFalse(report.ok)
        self.assertIn("no ARP reply", text(report))

    def test_a_request_that_is_not_a_broadcast_fails(self):
        unicast = arp(1, GATEWAY_MAC, GUEST, GUEST, GUEST_IP, bytes(6), GATEWAY_IP)
        report = analyze([unicast, reply()])
        self.assertFalse(report.ok)
        self.assertIn("not broadcast", text(report))

    def test_the_required_number_of_pairs(self):
        frames = [request(), reply()] * 3
        self.assertTrue(analyze(frames, min_arp_pairs=3).ok)
        report = analyze(frames, min_arp_pairs=4)
        self.assertFalse(report.ok)
        self.assertIn("3 complete ARP exchange(s)", text(report))

    def test_each_reply_answers_only_one_request(self):
        report = analyze([request(), request(), reply()], min_arp_pairs=2)
        self.assertFalse(report.ok)

    def test_interleaved_pairs_are_matched_in_order(self):
        self.assertTrue(analyze([request(), request(), reply(), reply()], min_arp_pairs=2).ok)


class Policy(unittest.TestCase):
    def test_frames_within_bounds_pass(self):
        self.assertTrue(analyze([request(), reply(), probe(14), probe(1514)]).ok)

    def test_a_runt_on_the_wire_fails(self):
        report = analyze([request(), reply(), probe(14)[:13]])
        self.assertFalse(report.ok)
        self.assertIn("shorter than an Ethernet header", text(report))

    def test_an_oversize_frame_on_the_wire_fails(self):
        report = analyze([request(), reply(), bytes(1515)])
        self.assertFalse(report.ok)
        self.assertIn("longer than MTU + 14", text(report))

    def test_a_frame_captured_truncated_fails(self):
        data = bytearray(pcap.write_pcap([request(), reply()]))
        # Make the first record claim more bytes on the wire than were kept.
        struct.pack_into("<I", data, 24 + 12, 100)
        report = ap.analyze(pcap.read_pcap(bytes(data)), guest_mac=GUEST, gateway_ip=GATEWAY_IP,
                            min_arp_pairs=1, expect_probe=False)
        self.assertFalse(report.ok)
        self.assertIn("truncated", text(report))


class Probe(unittest.TestCase):
    base = [request(), reply()]

    def test_the_two_legal_extremes_pass(self):
        report = analyze(self.base + [probe(14), probe(1514)], expect_probe=True)
        self.assertTrue(report.ok, text(report))
        self.assertIn("lengths on the wire=[14, 1514]", text(report))

    def test_a_dropped_legal_extreme_fails(self):
        for missing in (14, 1514):
            keep = [n for n in (14, 1514) if n != missing]
            report = analyze(self.base + [probe(n) for n in keep], expect_probe=True)
            self.assertFalse(report.ok, missing)
            self.assertIn(f"no {missing}-byte probe frame", text(report))

    def test_an_illegal_frame_leaking_fails(self):
        report = analyze(self.base + [probe(14), probe(1514), probe(1515)], expect_probe=True)
        self.assertFalse(report.ok)
        self.assertIn("1515-byte frame reached the wire", text(report))
        runt = probe(14)[:13]
        report = analyze(self.base + [probe(14), probe(1514), runt], expect_probe=True)
        self.assertFalse(report.ok)
        self.assertIn("13-byte frame reached the wire", text(report))

    def test_a_corrupted_payload_fails(self):
        damaged = bytearray(probe(1514))
        damaged[700] ^= 0xFF
        report = analyze(self.base + [probe(14), bytes(damaged)], expect_probe=True)
        self.assertFalse(report.ok)
        self.assertIn("payload differs at byte 700", text(report))

    def test_no_probe_frames_at_all_fails(self):
        report = analyze(self.base, expect_probe=True)
        self.assertFalse(report.ok)

    def test_a_probe_frame_of_another_length_fails(self):
        report = analyze(self.base + [probe(14), probe(1514), probe(200)], expect_probe=True)
        self.assertFalse(report.ok)
        self.assertIn("unexpected lengths", text(report))

    def test_the_probe_is_not_required_unless_asked_for(self):
        self.assertTrue(analyze(self.base, expect_probe=False).ok)


class Cli(unittest.TestCase):
    def run_main(self, data: bytes | None, *extra: str) -> int:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "net.pcap"
            if data is not None:
                path.write_bytes(data)
            return ap.main([str(path), *extra])

    def test_exit_codes(self):
        good = pcap.write_pcap([request(), reply(), probe(14), probe(1514)])
        self.assertEqual(self.run_main(good, "--expect-probe"), 0)
        self.assertEqual(self.run_main(pcap.write_pcap([request()])), 1)
        self.assertEqual(self.run_main(b""), 1, "an empty capture fails")
        self.assertEqual(self.run_main(good[:-5]), 1, "a truncated capture fails")
        self.assertEqual(self.run_main(None), 1, "a missing file fails")
        self.assertEqual(self.run_main(pcap.write_pcap([])), 1, "a capture of zero frames fails")

    def test_a_different_guest_mac_is_honoured(self):
        good = pcap.write_pcap([request(), reply()])
        self.assertEqual(self.run_main(good, "--guest-mac", "52:54:00:aa:bb:cc"), 1)


class Decoders(unittest.TestCase):
    def test_ipv4_checksum_and_icmp(self):
        header = bytes.fromhex("45000054000040004001000a0a000f0a00020202")[:20]
        header = bytearray(header)
        header[10:12] = b"\0\0"
        struct.pack_into(">H", header, 10, pcap.ipv4_checksum(bytes(header)))
        self.assertEqual(pcap.ipv4_checksum(bytes(header)), 0)
        icmp = bytearray(struct.pack(">BBHHH", 8, 0, 0, 7, 3) + b"abcdefgh")
        struct.pack_into(">H", icmp, 2, pcap.ipv4_checksum(bytes(icmp)))
        frame = GATEWAY_MAC + GUEST + b"\x08\x00" + bytes(header[:2]) + struct.pack(">H", 20 + len(icmp)) + bytes(header[4:]) + bytes(icmp)
        packet = pcap.parse_ipv4(frame)
        self.assertIsNotNone(packet)
        echo = pcap.parse_icmp_echo(packet)
        self.assertEqual((echo.type, echo.ident, echo.seq, echo.data), (8, 7, 3, b"abcdefgh"))
        self.assertTrue(echo.checksum_ok)

    def test_garbage_does_not_decode(self):
        self.assertIsNone(pcap.parse_arp(b""))
        self.assertIsNone(pcap.parse_ipv4(b"\0" * 60))
        self.assertIsNone(pcap.ethertype(b"\0" * 5))


if __name__ == "__main__":
    unittest.main()
