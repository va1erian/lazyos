"""IEEE 802.11 frames as the fake access point builds and reads them,
written from IEEE Std 802.11-2020 clause 9 independently of `libs/ieee80211`
(the point of the simulator is that the two sides do not share a mistake).

Frames start at the MAC header and carry no FCS.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass

BROADCAST = b"\xff" * 6

# Management subtypes (9.2.4.1.3).
ASSOC_REQ, ASSOC_RESP, REASSOC_REQ, PROBE_REQ, PROBE_RESP = 0, 1, 2, 4, 5
BEACON, DISASSOC, AUTH, DEAUTH, ACTION = 8, 10, 11, 12, 13
TYPE_MGMT, TYPE_DATA = 0, 2
# Frame control flags.
TO_DS, FROM_DS, PROTECTED, ORDER = 0x01, 0x02, 0x40, 0x80
# Element ids.
IE_SSID, IE_RATES, IE_DS, IE_HT_CAP, IE_RSN = 0, 1, 3, 45, 48
RATES = bytes([0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24])
#: WPA2-PSK with CCMP-128 for both ciphers, no capabilities (9.4.2.24).
RSN_WPA2_PSK = bytes([IE_RSN, 20, 1, 0, 0x00, 0x0F, 0xAC, 4, 1, 0, 0x00, 0x0F, 0xAC, 4,
                      1, 0, 0x00, 0x0F, 0xAC, 2, 0, 0])
CAP_ESS, CAP_PRIVACY = 0x0001, 0x0010
RFC1042 = bytes([0xAA, 0xAA, 0x03, 0, 0, 0])
ETHERTYPE_EAPOL = 0x888E


def mac_text(mac: bytes) -> str:
    return ":".join(f"{b:02x}" for b in mac)


def ie(element: int, body: bytes) -> bytes:
    return bytes([element, len(body)]) + body


def mgmt(subtype: int, da: bytes, sa: bytes, bssid: bytes, seq: int, body: bytes) -> bytes:
    return bytes([subtype << 4, 0]) + b"\0\0" + da + sa + bssid + struct.pack("<H", (seq & 0xFFF) << 4) + body


@dataclass
class Header:
    ftype: int
    subtype: int
    flags: int
    addr1: bytes
    addr2: bytes
    addr3: bytes
    seq: int
    qos: int | None
    length: int

    @property
    def protected(self) -> bool:
        return bool(self.flags & PROTECTED)

    @property
    def tid(self) -> int | None:
        return None if self.qos is None else self.qos & 0x0F


def parse_header(frame: bytes) -> Header | None:
    """The MAC header of a management or data frame (no four-address frames:
    the station never sends one)."""
    if len(frame) < 24 or frame[0] & 0x03:
        return None
    ftype, subtype, flags = (frame[0] >> 2) & 3, frame[0] >> 4, frame[1]
    if ftype not in (TYPE_MGMT, TYPE_DATA) or (flags & (TO_DS | FROM_DS)) == TO_DS | FROM_DS:
        return None
    length, qos = 24, None
    if ftype == TYPE_DATA and subtype & 0x08:
        if len(frame) < 26:
            return None
        qos = struct.unpack("<H", frame[24:26])[0]
        length = 26 + (4 if flags & ORDER else 0)
    if len(frame) < length:
        return None
    seq = struct.unpack("<H", frame[22:24])[0] >> 4
    return Header(ftype, subtype, flags, frame[4:10], frame[10:16], frame[16:22], seq, qos, length)


def elements(data: bytes) -> dict[int, bytes]:
    """The first of each element (enough for what an AP reads)."""
    out: dict[int, bytes] = {}
    at = 0
    while at + 2 <= len(data):
        element, length = data[at], data[at + 1]
        if at + 2 + length > len(data):
            break
        out.setdefault(element, data[at + 2:at + 2 + length])
        at += 2 + length
    return out


def beacon_body(ssid: bytes, channel: int, interval: int, privacy: bool, ht: bool,
                timestamp: int) -> bytes:
    capability = CAP_ESS | (CAP_PRIVACY if privacy else 0)
    body = struct.pack("<QHH", timestamp, interval, capability)
    body += ie(IE_SSID, ssid) + ie(IE_RATES, RATES) + ie(IE_DS, bytes([channel]))
    if ht:
        body += ie(IE_HT_CAP, bytes(26))
    if privacy:
        body += RSN_WPA2_PSK
    return body


def data_frame(da: bytes, bssid: bytes, sa: bytes, seq: int, payload: bytes, *, qos: bool,
               security: bytes | None = None, tid: int = 0) -> bytes:
    """A From-DS data frame (AP to station): `payload` is the LLC/SNAP MSDU,
    `security` a CCMP header placed before it (Protected set)."""
    flags = FROM_DS | (PROTECTED if security else 0)
    fc0 = (TYPE_DATA << 2) | ((8 if qos else 0) << 4)
    out = bytes([fc0, flags]) + b"\0\0" + da + bssid + sa + struct.pack("<H", (seq & 0xFFF) << 4)
    if qos:
        out += struct.pack("<H", tid & 0x0F)
    return out + (security or b"") + payload


def snap(ethertype: int, payload: bytes) -> bytes:
    return RFC1042 + struct.pack(">H", ethertype) + payload


def unsnap(msdu: bytes) -> tuple[int, bytes] | None:
    if len(msdu) < 8 or msdu[:6] != RFC1042:
        return None
    return struct.unpack(">H", msdu[6:8])[0], msdu[8:]
