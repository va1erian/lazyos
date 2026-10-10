#!/usr/bin/env python3
"""The multi-NIC judge fails when it should (`multi_judge.py`).

Synthetic captures of two cards: a correct run passes, and each way the stack
could get it wrong (traffic on the wrong card, no new DHCP after the link came
back, a half DHCP exchange, a DNS query to the loser's resolver) is reported.
Run: python tools/net/test_multi_judge.py
"""

from __future__ import annotations

import struct
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import multi_judge as mj  # noqa: E402
import pcap  # noqa: E402
from pcap_fixtures import dhcp_body, ip_packet, udp  # noqa: E402

MAC0 = bytes.fromhex("525400123456")
MAC1 = bytes.fromhex("525400123457")
GW0 = bytes.fromhex("525500000202")
GW1 = bytes.fromhex("525500000302")
NET0 = {"gw": bytes([10, 0, 2, 2]), "dns": bytes([10, 0, 2, 3]), "ip": bytes([10, 0, 2, 15])}
NET1 = {"gw": bytes([10, 0, 3, 2]), "dns": bytes([10, 0, 3, 3]), "ip": bytes([10, 0, 3, 15])}
TARGET = bytes([192, 0, 2, 55])
BROADCAST = b"\xff" * 6


def dhcp(kind: int, mac: bytes, gw_mac: bytes, net: dict, xid: int) -> bytes:
    from_client = kind in (1, 3)
    body = dhcp_body(1 if from_client else 2, xid, mac, bytes(4) if from_client else net["ip"], kind)
    if from_client:
        return BROADCAST + mac + b"\x08\x00" + ip_packet(bytes(4), b"\xff" * 4, 17, udp(68, 67, body))
    return BROADCAST + gw_mac + b"\x08\x00" + ip_packet(net["gw"], b"\xff" * 4, 17, udp(67, 68, body))


def exchange(mac: bytes, gw_mac: bytes, net: dict, xid: int) -> list[bytes]:
    return [dhcp(kind, mac, gw_mac, net, xid) for kind in (1, 2, 3, 5)]


def echo(mac: bytes, gw_mac: bytes, net: dict, dst: bytes) -> bytes:
    icmp = bytearray(struct.pack(">BBHHH", 8, 0, 0, 0x42, 1) + b"data")
    struct.pack_into(">H", icmp, 2, pcap.ipv4_checksum(bytes(icmp)))
    return gw_mac + mac + b"\x08\x00" + ip_packet(net["ip"], dst, 1, bytes(icmp))


def query(mac: bytes, gw_mac: bytes, net: dict) -> bytes:
    payload = udp(49152, 53, b"\x12\x34\x01\x00\x00\x01\x00\x00\x00\x00\x00\x00")
    return gw_mac + mac + b"\x08\x00" + ip_packet(net["ip"], net["dns"], 17, payload)


def capture(raw: list[bytes]) -> list[pcap.Frame]:
    """One frame per second from t=1000, so index i is at time 1000 + i."""
    return pcap.read_pcap(pcap.write_pcap(raw, start=1000.0, step=1.0))


class MultiJudge(unittest.TestCase):
    def cards(self, raw0, raw1):
        return {"eth0": (capture(raw0), MAC0), "eth1": (capture(raw1), MAC1)}

    def traffic(self, cards, winner, since=0.0, until=mj.FOREVER):
        return mj.phase_traffic(
            "phase", cards, winner, target="192.0.2.55",
            resolver={"eth0": "10.0.2.3", "eth1": "10.0.3.3"}, since=since, until=until,
        )

    def test_dhcp_on_both_cards_is_complete(self):
        for mac, gw, net in ((MAC0, GW0, NET0), (MAC1, GW1, NET1)):
            frames = capture(exchange(mac, gw, net, 7))
            self.assertEqual(mj.dhcp_cycles(frames, mac), 1)
            self.assertEqual(mj.phase_dhcp("boot", frames, mac, 0), [])

    def test_a_half_exchange_is_not_a_lease(self):
        frames = capture(exchange(MAC0, GW0, NET0, 7)[:3])
        self.assertEqual(mj.dhcp_cycles(frames, MAC0), 0)
        self.assertTrue(mj.phase_dhcp("boot", frames, MAC0, 0))
        # Messages of another client's transaction do not complete ours.
        mixed = capture([exchange(MAC0, GW0, NET0, 7)[0], *exchange(MAC1, GW1, NET1, 7)[1:]])
        self.assertEqual(mj.dhcp_cycles(mixed, MAC0), 0)

    def test_the_winner_carries_traffic_and_dns_and_the_loser_nothing(self):
        raw0 = [echo(MAC0, GW0, NET0, TARGET), query(MAC0, GW0, NET0)]
        cards = self.cards(raw0, [])
        self.assertEqual(self.traffic(cards, "eth0"), [])
        problems = self.traffic(cards, "eth1")
        self.assertEqual(len(problems), 4, problems)

    def test_traffic_on_the_loser_is_reported(self):
        both = [echo(MAC0, GW0, NET0, TARGET), query(MAC0, GW0, NET0)]
        leak = [echo(MAC1, GW1, NET1, TARGET)]
        problems = self.traffic(self.cards(both, leak), "eth0")
        self.assertEqual(len(problems), 1, problems)
        self.assertTrue(any("left by eth1, not eth0" in p for p in problems))
        # A DNS query to the loser's resolver, with no echo, is caught too.
        problems = self.traffic(self.cards(both, [query(MAC1, GW1, NET1)]), "eth0")
        self.assertEqual(len(problems), 1, problems)

    def test_windows_separate_the_phases(self):
        # Before the link goes down (t < 1002) eth0 carries traffic; after, eth1.
        raw0 = [echo(MAC0, GW0, NET0, TARGET), query(MAC0, GW0, NET0)]
        raw1 = [b"\0" * 60, b"\0" * 60, echo(MAC1, GW1, NET1, TARGET), query(MAC1, GW1, NET1)]
        cards = self.cards(raw0, raw1)
        self.assertEqual(self.traffic(cards, "eth0", until=1001.5), [])
        self.assertEqual(self.traffic(cards, "eth1", since=1002.0), [])
        # Judged over the wrong window, the failover looks like a leak.
        self.assertTrue(self.traffic(cards, "eth0"))

    def test_a_link_that_came_back_must_restart_dhcp(self):
        first = exchange(MAC0, GW0, NET0, 1)
        again = exchange(MAC0, GW0, NET0, 2)
        frames = capture(first + [b"\0" * 60] + again)
        after = 1000.0 + len(first)
        self.assertEqual(mj.phase_dhcp("up", frames, MAC0, after, restarted=True), [])
        # A bare renewal (REQUEST, ACK) is not starting over.
        renewal = [dhcp(3, MAC0, GW0, NET0, 3), dhcp(5, MAC0, GW0, NET0, 3)]
        frames = capture(first + [b"\0" * 60] + renewal)
        self.assertTrue(mj.phase_dhcp("up", frames, MAC0, after, restarted=True))
        # Nothing at all after the link came back.
        frames = capture(first)
        self.assertEqual(len(mj.phase_dhcp("up", frames, MAC0, 2000.0, restarted=True)), 2)

    def test_counting_helpers_ignore_other_cards_and_frames(self):
        frames = capture([echo(MAC1, GW1, NET1, TARGET), b"\0" * 8, query(MAC1, GW1, NET1)])
        self.assertEqual(mj.echo_requests(frames, MAC0, "192.0.2.55"), 0)
        self.assertEqual(mj.echo_requests(frames, MAC1, "192.0.2.55"), 1)
        self.assertEqual(mj.udp_queries(frames, MAC1, "10.0.3.3"), 1)
        self.assertEqual(mj.udp_queries(frames, MAC1, "10.0.2.3"), 0)
        self.assertEqual(mj.frames_from(frames, MAC1), 2)


if __name__ == "__main__":
    unittest.main()
