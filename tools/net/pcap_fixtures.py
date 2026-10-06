"""Synthetic frames for the pcap analyzer's tests (`test_analyze_pcap.py`,
`test_analyze_pcap_n2.py`): ARP, the length probe and (stage N2) DHCP and ICMP
echo, between the guest and QEMU's user-mode gateway.
"""

from __future__ import annotations

import struct
import sys
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
               dst: bytes = GATEWAY_IP, bad_ip: bool = False, reply_to: bytes = GUEST_ADDR) -> bytes:
    icmp = bytearray(struct.pack(">BBHHH", kind, 0, 0, ident, seq) + data)
    checksum = pcap.ipv4_checksum(bytes(icmp))
    struct.pack_into(">H", icmp, 2, checksum ^ 0x0F0F if bad_checksum else checksum)
    if from_guest:
        return GATEWAY_MAC + GUEST + b"\x08\x00" + ip_packet(GUEST_ADDR, dst, 1, bytes(icmp), bad_checksum=bad_ip)
    return GUEST + GATEWAY_MAC + b"\x08\x00" + ip_packet(GATEWAY_IP, reply_to, 1, bytes(icmp), bad_checksum=bad_ip)


def ping_pair(ident: int = 0x4242, seq: int = 1, data: bytes = b"payload!") -> list[bytes]:
    return [echo_frame(8, ident, seq, data, from_guest=True), echo_frame(0, ident, seq, data, from_guest=False)]


def analyze_n2(frames: list[bytes], *, dhcp: int = 0, pings: int = 0):
    parsed = pcap.read_pcap(pcap.write_pcap(frames))
    return ap.analyze(parsed, guest_mac=GUEST, gateway_ip=GATEWAY_IP, min_arp_pairs=0, expect_probe=False,
                      min_dhcp=dhcp, min_pings=pings)
