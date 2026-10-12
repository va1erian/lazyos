"""The simulated air (`libs/simair`, docs/wifi-prerequisites-plan.md 3.6).

Each 802.11 frame (MAC header to MIC, no FCS) travels in one Ethernet frame
of EtherType 0x88B5 on the guest's air card, which QEMU connects to this
process with a UDP socket backend (`-netdev socket,udp=...,localaddr=...`):
one datagram per Ethernet frame.

    Ethernet  dst ff:ff:ff:ff:ff:ff  src <sender>  type 0x88B5
    air       version 1, band, channel, rssi (i8), flags, reserved, length (u16 BE)
    802.11 frame

Also the pcap writer: every frame on the air, in both directions, with the
radiotap-free IEEE 802.11 link type (105), as a sniffer on that channel would
record it (protected frames stay encrypted).
"""

from __future__ import annotations

import struct
import time
from dataclasses import dataclass
from pathlib import Path

ETHERTYPE = 0x88B5
VERSION = 1
HEADER = 8
ETH_HEADER = 14
#: The air card's MTU, and the largest 802.11 frame it carries.
CARD_MTU = 1500
MAX_FRAME = CARD_MTU - HEADER
FLAG_BASIC_RATE = 1
#: The MAC the harness gives the guest's air card (`netpolicy::SIM_AIR_MAC`).
GUEST_AIR_MAC = bytes.fromhex("025753494d00")
#: The source address the AP side puts on its Ethernet frames.
AP_CARD_MAC = bytes.fromhex("025753494dfe")
LINKTYPE_IEEE802_11 = 105


@dataclass(frozen=True)
class Air:
    band: int
    channel: int
    rssi: int = 0
    flags: int = 0


def encode(src: bytes, air: Air, frame: bytes) -> bytes:
    """The Ethernet frame carrying `frame`."""
    if len(frame) > MAX_FRAME:
        raise ValueError(f"an 802.11 frame of {len(frame)} octets does not fit the air")
    header = struct.pack(">BBBbBBH", VERSION, air.band, air.channel, air.rssi, air.flags, 0, len(frame))
    return b"\xff" * 6 + src + struct.pack(">H", ETHERTYPE) + header + frame


def decode(eth: bytes) -> tuple[Air, bytes] | None:
    """The air header and 802.11 frame of an Ethernet frame, or None for
    anything else (the guest's card also sends ARP, IPv6 noise, ...)."""
    if len(eth) < ETH_HEADER + HEADER or struct.unpack(">H", eth[12:14])[0] != ETHERTYPE:
        return None
    version, band, channel, rssi, flags, _, length = struct.unpack(">BBBbBBH", eth[14:22])
    if version != VERSION or band > 1 or length > MAX_FRAME or len(eth) < 22 + length:
        return None
    return Air(band, channel, rssi, flags), eth[22:22 + length]


class PcapWriter:
    """A pcap of 802.11 frames (link type 105)."""

    def __init__(self, path: Path | None):
        self.file = None
        if path is not None:
            path.parent.mkdir(parents=True, exist_ok=True)
            self.file = open(path, "wb")
            self.file.write(struct.pack("<IHHiIII", 0xA1B2C3D4, 2, 4, 0, 0, 65535, LINKTYPE_IEEE802_11))
            self.file.flush()

    def write(self, frame: bytes) -> None:
        if self.file is None:
            return
        now = time.time()
        seconds = int(now)
        self.file.write(struct.pack("<IIII", seconds, int((now - seconds) * 1e6), len(frame), len(frame)))
        self.file.write(frame)
        self.file.flush()

    def close(self) -> None:
        if self.file is not None:
            self.file.close()
            self.file = None


def read_pcap(path: Path) -> list[bytes]:
    """Every frame of a pcap this module wrote (the judge's reader)."""
    data = path.read_bytes()
    if len(data) < 24:
        return []
    magic, = struct.unpack("<I", data[:4])
    if magic != 0xA1B2C3D4:
        raise ValueError(f"{path}: not a little-endian pcap")
    linktype, = struct.unpack("<I", data[20:24])
    if linktype != LINKTYPE_IEEE802_11:
        raise ValueError(f"{path}: link type {linktype}, not 802.11")
    frames, at = [], 24
    while at + 16 <= len(data):
        _, _, caplen, _ = struct.unpack("<IIII", data[at:at + 16])
        at += 16
        frames.append(data[at:at + caplen])
        at += caplen
    return frames
