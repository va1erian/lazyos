"""Read classic pcap captures and decode the few protocols the network checks
need (Ethernet, ARP, IPv4, ICMP, UDP, DHCP), using only the standard library.

QEMU's `filter-dump` writes a classic little-endian pcap with link type 1
(Ethernet). The reader is strict on purpose: a capture that does not parse
cleanly (empty file, bad magic, a record cut short) is an *error*, never
"fewer packets", so a killed emulator or a full disk cannot turn into a pass.
`write_pcap` builds captures for the analyzer's own tests.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass
from pathlib import Path

MAGIC_LE = 0xA1B2C3D4
MAGIC_LE_NS = 0xA1B23C4D
LINKTYPE_ETHERNET = 1
#: Records larger than this are refused instead of allocated.
MAX_RECORD = 1 << 20

ETHERTYPE_ARP = 0x0806
ETHERTYPE_IPV4 = 0x0800


class PcapError(Exception):
    """The capture is unusable (empty, malformed or truncated)."""


@dataclass(frozen=True)
class Frame:
    """One captured frame: its position in the capture and its bytes."""

    index: int
    time: float
    data: bytes
    orig_len: int

    @property
    def length(self) -> int:
        return len(self.data)

    @property
    def truncated(self) -> bool:
        """The capture kept fewer bytes than were on the wire."""
        return len(self.data) < self.orig_len


def read_pcap(source: bytes | str | Path) -> list[Frame]:
    """Parse a classic pcap. Raises `PcapError` for anything not well formed."""
    data = Path(source).read_bytes() if isinstance(source, (str, Path)) else bytes(source)
    if not data:
        raise PcapError("the capture file is empty")
    if len(data) < 24:
        raise PcapError(f"the capture is {len(data)} bytes, shorter than a pcap header")
    magic = struct.unpack_from("<I", data, 0)[0]
    if magic == MAGIC_LE:
        scale = 1e-6
    elif magic == MAGIC_LE_NS:
        scale = 1e-9
    else:
        raise PcapError(f"bad pcap magic {magic:#010x} (only little-endian classic pcap is supported)")
    linktype = struct.unpack_from("<I", data, 20)[0]
    if linktype != LINKTYPE_ETHERNET:
        raise PcapError(f"link type {linktype}, expected Ethernet (1)")
    frames: list[Frame] = []
    at = 24
    while at < len(data):
        if len(data) - at < 16:
            raise PcapError(f"truncated capture: {len(data) - at} stray bytes after record {len(frames)}")
        sec, frac, caplen, orig = struct.unpack_from("<IIII", data, at)
        at += 16
        if caplen > MAX_RECORD:
            raise PcapError(f"record {len(frames)} claims {caplen} captured bytes")
        if len(data) - at < caplen:
            raise PcapError(
                f"truncated capture: record {len(frames)} needs {caplen} bytes, {len(data) - at} remain"
            )
        frames.append(Frame(len(frames), sec + frac * scale, data[at : at + caplen], orig))
        at += caplen
    return frames


def write_pcap(frames: list[bytes], *, start: float = 1000.0, step: float = 0.001) -> bytes:
    """A little-endian classic pcap holding `frames` (for tests)."""
    out = bytearray(struct.pack("<IHHiIII", MAGIC_LE, 2, 4, 0, 0, 65535, LINKTYPE_ETHERNET))
    for i, frame in enumerate(frames):
        stamp = start + i * step
        sec = int(stamp)
        out += struct.pack("<IIII", sec, int((stamp - sec) * 1e6), len(frame), len(frame))
        out += frame
    return bytes(out)


# ---- decoding -----------------------------------------------------------------


def mac_text(mac: bytes) -> str:
    return ":".join(f"{b:02x}" for b in mac)


def parse_mac(text: str) -> bytes:
    parts = text.split(":")
    if len(parts) != 6:
        raise ValueError(f"not a MAC address: {text!r}")
    return bytes(int(p, 16) for p in parts)


def ip_text(ip: bytes) -> str:
    return ".".join(str(b) for b in ip)


def parse_ip(text: str) -> bytes:
    parts = text.split(".")
    if len(parts) != 4:
        raise ValueError(f"not an IPv4 address: {text!r}")
    return bytes(int(p) for p in parts)


def ethertype(frame: bytes) -> int | None:
    return struct.unpack_from(">H", frame, 12)[0] if len(frame) >= 14 else None


@dataclass(frozen=True)
class Arp:
    op: int
    sender_mac: bytes
    sender_ip: bytes
    target_mac: bytes
    target_ip: bytes
    eth_dst: bytes
    eth_src: bytes


def parse_arp(frame: bytes) -> Arp | None:
    """The ARP (IPv4 over Ethernet) message in `frame`, or None if it is not one."""
    if len(frame) < 42 or ethertype(frame) != ETHERTYPE_ARP:
        return None
    htype, ptype, hlen, plen, op = struct.unpack_from(">HHBBH", frame, 14)
    if (htype, ptype, hlen, plen) != (1, ETHERTYPE_IPV4, 6, 4):
        return None
    return Arp(op, frame[22:28], frame[28:32], frame[32:38], frame[38:42], frame[0:6], frame[6:12])


@dataclass(frozen=True)
class Ipv4:
    src: bytes
    dst: bytes
    proto: int
    ttl: int
    ident: int
    header_ok: bool
    payload: bytes
    eth_src: bytes
    eth_dst: bytes


def ipv4_checksum(header: bytes) -> int:
    if len(header) % 2:
        header += b"\0"
    total = sum(struct.unpack(f">{len(header) // 2}H", header))
    while total >> 16:
        total = (total & 0xFFFF) + (total >> 16)
    return (~total) & 0xFFFF


def parse_ipv4(frame: bytes) -> Ipv4 | None:
    """The IPv4 packet in `frame` (header checksum verified, payload cut to the
    packet's own length), or None if it is not IPv4."""
    if len(frame) < 34 or ethertype(frame) != ETHERTYPE_IPV4:
        return None
    ver_ihl = frame[14]
    ihl = (ver_ihl & 0xF) * 4
    if ver_ihl >> 4 != 4 or ihl < 20 or len(frame) < 14 + ihl:
        return None
    total = struct.unpack_from(">H", frame, 16)[0]
    ident = struct.unpack_from(">H", frame, 18)[0]
    header = frame[14 : 14 + ihl]
    end = 14 + total if 14 + ihl <= 14 + total <= len(frame) else len(frame)
    return Ipv4(
        src=frame[26:30], dst=frame[30:34], proto=frame[23], ttl=frame[22], ident=ident,
        header_ok=ipv4_checksum(header) == 0, payload=frame[14 + ihl : end],
        eth_src=frame[6:12], eth_dst=frame[0:6],
    )


@dataclass(frozen=True)
class Icmp:
    type: int
    code: int
    ident: int
    seq: int
    data: bytes
    checksum_ok: bool


def parse_icmp_echo(packet: Ipv4) -> Icmp | None:
    """The ICMP echo request/reply in `packet`, or None for anything else."""
    if packet.proto != 1 or len(packet.payload) < 8:
        return None
    kind, code, _, ident, seq = struct.unpack_from(">BBHHH", packet.payload, 0)
    if kind not in (0, 8):
        return None
    return Icmp(kind, code, ident, seq, packet.payload[8:], ipv4_checksum(packet.payload) == 0)


@dataclass(frozen=True)
class Dhcp:
    op: int
    xid: int
    client_mac: bytes
    yiaddr: bytes
    message_type: int | None
    options: dict
    src_port: int
    dst_port: int


def parse_dhcp(packet: Ipv4) -> Dhcp | None:
    """The DHCP message in a UDP packet on ports 67/68, or None."""
    if packet.proto != 17 or len(packet.payload) < 8 + 240:
        return None
    src, dst, _, _ = struct.unpack_from(">HHHH", packet.payload, 0)
    if {src, dst} != {67, 68}:
        return None
    body = packet.payload[8:]
    if body[236:240] != b"\x63\x82\x53\x63":
        return None
    xid = struct.unpack_from(">I", body, 4)[0]
    options: dict[int, bytes] = {}
    at = 240
    while at < len(body):
        code = body[at]
        if code == 255:
            break
        if code == 0:
            at += 1
            continue
        if at + 1 >= len(body) or at + 2 + body[at + 1] > len(body):
            return None
        options[code] = body[at + 2 : at + 2 + body[at + 1]]
        at += 2 + body[at + 1]
    kind = options.get(53, b"")
    return Dhcp(body[0], xid, body[28:34], body[16:20], kind[0] if kind else None, options, src, dst)
