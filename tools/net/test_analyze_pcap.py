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

GUEST = bytes.fromhex("525400123456")
GATEWAY_MAC = bytes.fromhex("525500000202")
GATEWAY_IP = bytes([10, 0, 2, 2])
GUEST_IP = bytes([10, 0, 2, 15])
BROADCAST = b"\xff" * 6


def arp(op: int, eth_dst: bytes, eth_src: bytes, sender_mac: bytes, sender_ip: bytes,
        target_mac: bytes, target_ip: bytes) -> bytes:
    return (eth_dst + eth_src + struct.pack(">H", 0x0806)
            + struct.pack(">HHBBH", 1, 0x0800, 6, 4, op) + sender_mac + sender_ip + target_mac + target_ip)


def request(target: bytes = GATEWAY_IP) -> bytes:
    return arp(1, BROADCAST, GUEST, GUEST, GUEST_IP, bytes(6), target)


def reply(sender_ip: bytes = GATEWAY_IP) -> bytes:
    return arp(2, GUEST, GATEWAY_MAC, GATEWAY_MAC, sender_ip, GUEST, GUEST_IP)


def probe(length: int) -> bytes:
    frame = bytearray(BROADCAST + GUEST + b"\x88\xb5")
    frame += bytes((i ^ 0x5A) & 0xFF for i in range(14, length))
    return bytes(frame[:length])


def analyze(frames: list[bytes], **kw):
    parsed = pcap.read_pcap(pcap.write_pcap(frames))
    options = dict(guest_mac=GUEST, gateway_ip=GATEWAY_IP, min_arp_pairs=1, expect_probe=False)
    options.update(kw)
    return ap.analyze(parsed, **options)


def text(report) -> str:
    return "\n".join(report.lines)


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


# ---- stage N2: DHCP, IP sanity and ICMP echo -----------------------------------------

GUEST_ADDR = bytes([10, 0, 2, 15])


def ip_packet(src: bytes, dst: bytes, proto: int, payload: bytes, *, bad_checksum: bool = False, length_delta: int = 0) -> bytes:
    header = bytearray(struct.pack(">BBHHHBBH", 0x45, 0, 20 + len(payload) + length_delta, 1, 0, 64, proto, 0) + src + dst)
    checksum = pcap.ipv4_checksum(bytes(header))
    struct.pack_into(">H", header, 10, checksum ^ 0x00FF if bad_checksum else checksum)
    return bytes(header) + payload


def udp(sport: int, dport: int, payload: bytes) -> bytes:
    return struct.pack(">HHHH", sport, dport, 8 + len(payload), 0) + payload


def dhcp_body(op: int, xid: int, chaddr: bytes, yiaddr: bytes, kind: int) -> bytes:
    body = bytearray(240)
    body[0], body[1], body[2] = op, 1, 6
    struct.pack_into(">I", body, 4, xid)
    body[16:20] = yiaddr
    body[28:34] = chaddr
    body[236:240] = b"\x63\x82\x53\x63"
    return bytes(body) + bytes([53, 1, kind, 255])


def dhcp_frame(kind: int, xid: int = 0xABCD0001, *, chaddr: bytes = GUEST, yiaddr: bytes = GUEST_ADDR) -> bytes:
    from_client = kind in (1, 3)
    body = dhcp_body(1 if from_client else 2, xid, chaddr, yiaddr if not from_client else bytes(4), kind)
    if from_client:
        packet = ip_packet(bytes(4), b"\xff" * 4, 17, udp(68, 67, body))
        return BROADCAST + GUEST + b"\x08\x00" + packet
    packet = ip_packet(GATEWAY_IP, b"\xff" * 4, 17, udp(67, 68, body))
    return BROADCAST + GATEWAY_MAC + b"\x08\x00" + packet


def dhcp_exchange(xid: int = 0xABCD0001) -> list[bytes]:
    return [dhcp_frame(1, xid), dhcp_frame(2, xid), dhcp_frame(3, xid), dhcp_frame(5, xid)]


def echo_frame(kind: int, ident: int, seq: int, data: bytes, *, from_guest: bool, bad_checksum: bool = False,
               dst: bytes = GATEWAY_IP, bad_ip: bool = False) -> bytes:
    icmp = bytearray(struct.pack(">BBHHH", kind, 0, 0, ident, seq) + data)
    checksum = pcap.ipv4_checksum(bytes(icmp))
    struct.pack_into(">H", icmp, 2, checksum ^ 0x0F0F if bad_checksum else checksum)
    if from_guest:
        return GATEWAY_MAC + GUEST + b"\x08\x00" + ip_packet(GUEST_ADDR, dst, 1, bytes(icmp), bad_checksum=bad_ip)
    return GUEST + GATEWAY_MAC + b"\x08\x00" + ip_packet(GATEWAY_IP, GUEST_ADDR, 1, bytes(icmp), bad_checksum=bad_ip)


def ping_pair(ident: int = 0x4242, seq: int = 1, data: bytes = b"payload!") -> list[bytes]:
    return [echo_frame(8, ident, seq, data, from_guest=True), echo_frame(0, ident, seq, data, from_guest=False)]


def analyze_n2(frames: list[bytes], *, dhcp: int = 0, pings: int = 0):
    parsed = pcap.read_pcap(pcap.write_pcap(frames))
    return ap.analyze(parsed, guest_mac=GUEST, gateway_ip=GATEWAY_IP, min_arp_pairs=0, expect_probe=False,
                      min_dhcp=dhcp, min_pings=pings)


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
