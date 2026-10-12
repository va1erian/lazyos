#!/usr/bin/env python3
"""Host tests of the fake access point (`fake_ap.py`): a scripted station
walks it through probe, open and WPA2 joins, DHCP, pings, CCMP replays, a
wrong passphrase, a group rekey, deauthentication and silence.

    python tools/wifi/test_fake_ap.py
"""

from __future__ import annotations

import struct
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from fakeap import air, ccmp, dot11, netsvc, wpa  # noqa: E402
from fakeap.ap import World  # noqa: E402
import fake_ap  # noqa: E402

STA = bytes.fromhex("025753494d00")


class Harness:
    def __init__(self, passphrase: str = fake_ap.WPA2_PASSPHRASE):
        self.sent: list[tuple[air.Air, bytes]] = []
        self.lines: list[str] = []
        self.world = World(fake_ap.scenario(passphrase), lambda a, f: self.sent.append((a, f)),
                           air.PcapWriter(None), self.lines.append)
        self.now = 100.0

    def rx(self, channel: int, frame: bytes) -> list[bytes]:
        self.sent.clear()
        self.world.receive(air.Air(0, channel, 0, 0), frame, self.now)
        return [f for _, f in self.sent]

    def bss(self, ssid: str):
        return next(b for b in self.world.bsses if b.ssid == ssid)

    def auth_assoc(self, ssid: str, rsn: bytes | None = None) -> list[bytes]:
        bss = self.bss(ssid)
        out = self.rx(bss.channel, dot11.mgmt(dot11.AUTH, bss.bssid, STA, bss.bssid, 1, struct.pack("<HHH", 0, 1, 0)))
        assert struct.unpack("<HHH", out[0][24:30]) == (0, 2, 0)
        ies = dot11.ie(dot11.IE_SSID, ssid.encode()) + dot11.ie(dot11.IE_RATES, dot11.RATES) + (rsn or b"")
        return self.rx(bss.channel, dot11.mgmt(dot11.ASSOC_REQ, bss.bssid, STA, bss.bssid, 2,
                                               struct.pack("<HH", 0x0011, 10) + ies))

    def to_ds(self, ssid: str, ethertype: int, payload: bytes, tk: bytes | None = None, pn: int = 0,
              dst: bytes = netsvc.GATEWAY_MAC) -> list[bytes]:
        bss = self.bss(ssid)
        security = ccmp.ccmp_header(pn, 0) if tk else b""
        frame = bytes([0x88, dot11.TO_DS | (dot11.PROTECTED if tk else 0)]) + b"\0\0" + bss.bssid + STA + dst
        frame += struct.pack("<HH", 5 << 4, 0) + security + dot11.snap(ethertype, payload)
        if tk:
            frame = ccmp.seal(tk, frame, 26)
        return self.rx(bss.channel, frame)


def body_of(frame: bytes, tk: bytes | None = None) -> tuple[int, bytes]:
    h = dot11.parse_header(frame)
    body = ccmp.open_frame(tk, frame, h.length) if h.protected else frame[h.length:]
    assert body is not None, "the AP's frame did not decrypt"
    return dot11.unsnap(body)


def dhcp(kind: int, requested: bytes = b"") -> bytes:
    msg = bytes([1, 1, 6, 0]) + b"xid1" + bytes(20) + STA + bytes(10 + 192) + netsvc.DHCP_MAGIC
    msg += bytes([53, 1, kind]) + (bytes([50, 4]) + requested if requested else b"") + b"\xff"
    return netsvc.ipv4(bytes(4), b"\xff" * 4, 17, netsvc.udp(68, 67, msg))


class FakeApTest(unittest.TestCase):
    def test_crypto_known_answers(self):
        ccmp.self_test()
        wpa.self_test()

    def test_beacons_and_probes(self):
        h = Harness()
        h.world.tick(h.now)
        ssids = [dot11.elements(f[36:]).get(0) for _, f in h.sent]
        self.assertIn(b"LazyOpen", ssids)
        self.assertIn(b"", ssids, "the hidden network beacons without its name")
        probe = dot11.mgmt(dot11.PROBE_REQ, dot11.BROADCAST, STA, dot11.BROADCAST, 1, dot11.ie(0, b""))
        names = [dot11.elements(f[36:])[0] for f in h.rx(11, probe)]
        self.assertEqual(names, [b"LazyRoam"], "a wildcard probe does not reveal a hidden SSID")
        named = dot11.mgmt(dot11.PROBE_REQ, dot11.BROADCAST, STA, dot11.BROADCAST, 1, dot11.ie(0, b"LazyHidden"))
        self.assertEqual([dot11.elements(f[36:])[0] for f in h.rx(11, named)], [b"LazyHidden"])
        self.assertEqual(h.rx(6, named), [], "nothing on another channel")

    def test_open_join_dhcp_and_ping(self):
        h = Harness()
        (resp,) = h.auth_assoc("LazyOpen")
        self.assertEqual(struct.unpack("<HHH", resp[24:30])[1:], (0, 0xC001))
        self.assertTrue(any(line.startswith("AP:AUTHORIZED ssid=LazyOpen") for line in h.lines))
        (offer,) = h.to_ds("LazyOpen", netsvc.ETH_IP, dhcp(1), dst=dot11.BROADCAST)
        self.assertEqual(offer[4:10], dot11.BROADCAST)
        self.assertFalse(offer[1] & dot11.PROTECTED)
        (ack,) = h.to_ds("LazyOpen", netsvc.ETH_IP, dhcp(3, bytes([192, 168, 77, 100])), dst=dot11.BROADCAST)
        self.assertIn("AP:DHCP:ACK ip=192.168.77.100 ssid=LazyOpen", h.lines)
        h.sent.clear()
        h.world.tick(h.now + 2)
        pings = [f for _, f in h.sent if f[0] == 0x88]
        self.assertEqual(len(pings), 1)
        ethertype, packet = body_of(pings[0])
        self.assertEqual((ethertype, packet[9], packet[20]), (netsvc.ETH_IP, 1, 8))

    def handshake(self, h: Harness):
        (resp, m1) = h.auth_assoc("LazyWPA2", dot11.RSN_WPA2_PSK)
        bss = h.bss("LazyWPA2")
        _, eapol = body_of(m1)
        k1 = wpa.parse(eapol)
        self.assertEqual(k1.info, wpa.MSG1)
        snonce = bytes(range(32))
        ptk = wpa.ptk(wpa.pmk(fake_ap.WPA2_PASSPHRASE, b"LazyWPA2"), bss.bssid, STA, k1.nonce, snonce)
        m2 = wpa.build(wpa.VERSION_AES | wpa.PAIRWISE | wpa.MIC, 0, k1.replay, nonce=snonce,
                       data=dot11.RSN_WPA2_PSK, kck=ptk.kck)
        (m3_frame,) = h.to_ds("LazyWPA2", dot11.ETHERTYPE_EAPOL, m2, dst=bss.bssid)
        k3 = wpa.parse(body_of(m3_frame)[1])
        self.assertEqual(k3.info, wpa.MSG3)
        self.assertTrue(wpa.verify(ptk.kck, k3))
        m4 = wpa.build(wpa.VERSION_AES | wpa.PAIRWISE | wpa.MIC | wpa.SECURE, 0, k3.replay, kck=ptk.kck)
        h.to_ds("LazyWPA2", dot11.ETHERTYPE_EAPOL, m4, dst=bss.bssid)
        self.assertIn("AP:PTK:INSTALLED ssid=LazyWPA2", h.lines)
        return ptk

    def test_wpa2_join_ccmp_replay_and_rekey(self):
        h = Harness()
        ptk = self.handshake(h)
        (offer,) = h.to_ds("LazyWPA2", netsvc.ETH_IP, dhcp(1), tk=ptk.tk, pn=1, dst=dot11.BROADCAST)
        self.assertTrue(offer[1] & dot11.PROTECTED)
        self.assertEqual(ccmp.parse_ccmp_header(offer[26:])[1], 1, "the group key, id 1")
        self.assertIsNotNone(ccmp.open_frame(h.bss("LazyWPA2").gtk, offer, 26))
        self.assertEqual(h.to_ds("LazyWPA2", netsvc.ETH_IP, dhcp(1), tk=ptk.tk, pn=1, dst=dot11.BROADCAST), [])
        self.assertIn("AP:CCMP:REPLAY pn=1 ssid=LazyWPA2", h.lines)
        self.assertEqual(h.to_ds("LazyWPA2", netsvc.ETH_IP, dhcp(1), tk=bytes(16), pn=2), [])
        self.assertTrue(any(line.startswith("AP:CCMP:BADMIC") for line in h.lines))
        self.assertEqual(h.to_ds("LazyWPA2", netsvc.ETH_IP, dhcp(1)), [])
        self.assertIn("AP:DATA:PLAINTEXT:DROPPED", h.lines)
        h.sent.clear()
        h.world.fault("rekey", h.now)
        (g1,) = [f for _, f in h.sent]
        g1_key = wpa.parse(body_of(g1, ptk.tk)[1])
        self.assertEqual(g1_key.info, wpa.GROUP1)
        g2 = wpa.build(wpa.VERSION_AES | wpa.MIC | wpa.SECURE, 0, g1_key.replay, kck=ptk.kck)
        h.to_ds("LazyWPA2", dot11.ETHERTYPE_EAPOL, g2, tk=ptk.tk, pn=3, dst=h.bss("LazyWPA2").bssid)
        self.assertIn("AP:REKEY:G2:OK index=2 ssid=LazyWPA2", h.lines)
        self.assertEqual(h.bss("LazyWPA2").gtk_index, 2)

    def test_wrong_passphrase_is_refused(self):
        h = Harness(passphrase="another-passphrase")
        with self.assertRaises((AssertionError, ValueError)):
            self.handshake(h)
        self.assertIn("AP:EAPOL:M2:BADMIC ssid=LazyWPA2", h.lines)
        self.assertFalse(any(line.startswith("AP:PTK:INSTALLED") for line in h.lines))

    def test_assoc_refusals_and_faults(self):
        h = Harness()
        (resp,) = h.auth_assoc("LazyWPA2", None)
        self.assertEqual(struct.unpack("<H", resp[26:28])[0], 43, "WPA2 without an RSN element")
        h = Harness()
        h.auth_assoc("LazyOpen")
        h.world.fault("silence 3", h.now)
        h.sent.clear()
        h.world.tick(h.now + 1)
        self.assertEqual(h.sent, [], "a silent AP sends no beacon")
        h.world.tick(h.now + 4)
        self.assertTrue(h.sent)
        h.sent.clear()
        h.world.fault("deauth 7", h.now + 5)
        (deauth,) = [f for _, f in h.sent]
        self.assertEqual((deauth[0], struct.unpack("<H", deauth[24:26])[0]), (0xC0, 7))
        self.assertEqual(fake_ap.parse_schedule("rekey:8,silence 4:2"), [(2.0, "silence 4"), (8.0, "rekey")])

    def test_air_format_matches_simair(self):
        eth = air.encode(air.AP_CARD_MAC, air.Air(0, 6, -42, 1), b"frame")
        self.assertEqual(eth[12:22].hex(), "88b5010006d601000005")
        self.assertEqual(air.decode(eth), (air.Air(0, 6, -42, 1), b"frame"))
        self.assertIsNone(air.decode(eth[:21]))


if __name__ == "__main__":
    unittest.main()
