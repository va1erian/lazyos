"""Synthetic captures for the socket evidence checker's tests
(`test_sockets_pcap.py`, `test_sockets_ftp.py`): a guest talking to a gateway
echo server over TCP and UDP, DNS queries and FTP-style streams, frame by frame.
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pcap  # noqa: E402
import sockets_pcap as sp  # noqa: E402

GUEST = bytes.fromhex("525400123456")
GATEWAY_MAC = bytes.fromhex("525500000202")
GUEST_IP = bytes([10, 0, 2, 15])
GATEWAY_IP = bytes([10, 0, 2, 2])
DNS_IP = bytes([10, 0, 2, 3])
PORT = 47771


def checksum(data: bytes) -> int:
    return pcap.ipv4_checksum(data)


def ip_packet(src: bytes, dst: bytes, proto: int, payload: bytes) -> bytes:
    header = bytearray(struct.pack(">BBHHHBBH4s4s", 0x45, 0, 20 + len(payload), 1, 0, 64, proto, 0, src, dst))
    header[10:12] = struct.pack(">H", checksum(bytes(header)))
    return bytes(header) + payload


def with_pseudo(src: bytes, dst: bytes, proto: int, segment: bytearray, csum_at: int) -> bytes:
    segment[csum_at:csum_at + 2] = b"\0\0"
    pseudo = src + dst + struct.pack(">BBH", 0, proto, len(segment))
    segment[csum_at:csum_at + 2] = struct.pack(">H", checksum(pseudo + bytes(segment)))
    return bytes(segment)


def tcp_frame(from_guest: bool, sport: int, dport: int, seq: int, ack: int, flags: int, payload: bytes = b"",
              *, break_checksum: bool = False) -> bytes:
    src, dst = (GUEST_IP, GATEWAY_IP) if from_guest else (GATEWAY_IP, GUEST_IP)
    seg = bytearray(struct.pack(">HHIIHHHH", sport, dport, seq, ack, (5 << 12) | flags, 65535, 0, 0)) + payload
    data = with_pseudo(src, dst, 6, seg, 16)
    if break_checksum:
        data = data[:16] + bytes([data[16] ^ 0xFF]) + data[17:]
    eth = (GATEWAY_MAC + GUEST if not from_guest else GATEWAY_MAC + GUEST)
    mac_dst, mac_src = (GATEWAY_MAC, GUEST) if from_guest else (GUEST, GATEWAY_MAC)
    del eth
    return mac_dst + mac_src + struct.pack(">H", 0x0800) + ip_packet(src, dst, 6, data)


def udp_frame(from_guest: bool, sport: int, dport: int, payload: bytes, src_ip=None, dst_ip=None) -> bytes:
    src = src_ip or (GUEST_IP if from_guest else GATEWAY_IP)
    dst = dst_ip or (GATEWAY_IP if from_guest else GUEST_IP)
    seg = bytearray(struct.pack(">HHHH", sport, dport, 8 + len(payload), 0)) + payload
    data = with_pseudo(src, dst, 17, seg, 6)
    mac_dst, mac_src = (GATEWAY_MAC, GUEST) if from_guest else (GUEST, GATEWAY_MAC)
    return mac_dst + mac_src + struct.pack(">H", 0x0800) + ip_packet(src, dst, 17, data)


def echo_flow(sport: int, payload: bytes, *, fin_gateway: bool = True, fin_guest: bool = True,
              flip: int | None = None, drop_echo_tail: int = 0, handshake: bool = True,
              dport: int = PORT) -> list[bytes]:
    """One complete echo connection, in the order the wire would show it."""
    g, h = 1000, 5000
    out = []
    if handshake:
        out += [tcp_frame(True, sport, dport, g, 0, sp.SYN),
                tcp_frame(False, dport, sport, h, g + 1, sp.SYN | sp.ACK),
                tcp_frame(True, sport, dport, g + 1, h + 1, sp.ACK)]
    else:
        out += [tcp_frame(True, sport, dport, g, 0, sp.SYN)]
    gseq, hseq = g + 1, h + 1
    echoed = bytearray(payload)
    if flip is not None:
        echoed[flip] ^= 0x01
    if drop_echo_tail:
        echoed = echoed[:-drop_echo_tail]
    for at in range(0, len(payload), 1400):
        chunk = payload[at:at + 1400]
        out.append(tcp_frame(True, sport, dport, gseq, hseq, sp.ACK | sp.PSH, chunk))
        gseq += len(chunk)
        back = bytes(echoed[at:at + 1400])
        if back:
            out.append(tcp_frame(False, dport, sport, hseq, gseq, sp.ACK | sp.PSH, back))
            hseq += len(back)
    if fin_guest:
        out.append(tcp_frame(True, sport, dport, gseq, hseq, sp.FIN | sp.ACK))
        gseq += 1
    if fin_gateway:
        out.append(tcp_frame(False, dport, sport, hseq, gseq, sp.FIN | sp.ACK))
        hseq += 1
    out.append(tcp_frame(True, sport, dport, gseq, hseq, sp.ACK))
    return out


def frames_of(raw: list[bytes]) -> list[pcap.Frame]:
    return pcap.read_pcap(pcap.write_pcap(raw))


def dns_query(name: str, txid: int = 0x1234, qtype: int = 1) -> bytes:
    labels = b"".join(bytes([len(p)]) + p.encode() for p in name.split("."))
    return struct.pack(">HHHHHH", txid, 0x0100, 1, 0, 0, 0) + labels + b"\0" + struct.pack(">HH", qtype, 1)


def check_flows(raw, servers, min_flows=1):
    return sp.check_echo_flows(frames_of(raw), GUEST_IP, GATEWAY_IP, PORT, min_flows, servers)


def stream_flow(sport: int, dport: int, up: bytes, down: bytes, *, fin_gateway: bool = True,
                fin_guest: bool = True) -> list[bytes]:
    """A connection carrying `up` (guest to gateway) and then `down` bytes."""
    g, h = 1000, 5000
    out = [tcp_frame(True, sport, dport, g, 0, sp.SYN),
           tcp_frame(False, dport, sport, h, g + 1, sp.SYN | sp.ACK),
           tcp_frame(True, sport, dport, g + 1, h + 1, sp.ACK)]
    gseq, hseq = g + 1, h + 1
    for at in range(0, len(up), 1400):
        part = up[at:at + 1400]
        out.append(tcp_frame(True, sport, dport, gseq, hseq, sp.ACK | sp.PSH, part))
        gseq += len(part)
    for at in range(0, len(down), 1400):
        part = down[at:at + 1400]
        out.append(tcp_frame(False, dport, sport, hseq, gseq, sp.ACK | sp.PSH, part))
        hseq += len(part)
    if fin_guest:
        out.append(tcp_frame(True, sport, dport, gseq, hseq, sp.FIN | sp.ACK))
        gseq += 1
    if fin_gateway:
        out.append(tcp_frame(False, dport, sport, hseq, gseq, sp.FIN | sp.ACK))
    return out
