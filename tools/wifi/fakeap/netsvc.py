"""What the fake access point's network offers a station once its port is
open: ARP for the gateway, a DHCP server, and ICMP echo both ways (it
answers pings and sends its own, so data crosses the link in both
directions without a client in the guest).

Everything works on Ethernet frames; the AP converts to and from 802.11.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass, field

GATEWAY_MAC = bytes.fromhex("02a0000000fe")
GATEWAY = bytes([192, 168, 77, 1])
FIRST_LEASE = 100
NETMASK = bytes([255, 255, 255, 0])
LEASE_SECONDS = 3600
BROADCAST_MAC = b"\xff" * 6
ETH_IP, ETH_ARP = 0x0800, 0x0806
DHCP_MAGIC = bytes([99, 130, 83, 99])


def ip_text(ip: bytes) -> str:
    return ".".join(str(b) for b in ip)


def checksum(data: bytes) -> int:
    if len(data) % 2:
        data += b"\0"
    total = sum(struct.unpack(f">{len(data) // 2}H", data))
    while total >> 16:
        total = (total & 0xFFFF) + (total >> 16)
    return ~total & 0xFFFF


def ethernet(dst: bytes, src: bytes, ethertype: int, payload: bytes) -> bytes:
    return dst + src + struct.pack(">H", ethertype) + payload


def ipv4(src: bytes, dst: bytes, proto: int, payload: bytes, ident: int = 0) -> bytes:
    header = struct.pack(">BBHHHBBH4s4s", 0x45, 0, 20 + len(payload), ident, 0, 64, proto, 0, src, dst)
    header = header[:10] + struct.pack(">H", checksum(header)) + header[12:]
    return header + payload


def udp(sport: int, dport: int, payload: bytes) -> bytes:
    # A zero checksum: "none" for IPv4 UDP.
    return struct.pack(">HHHH", sport, dport, 8 + len(payload), 0) + payload


def icmp_echo(kind: int, ident: int, seq: int, data: bytes) -> bytes:
    packet = struct.pack(">BBHHH", kind, 0, 0, ident, seq) + data
    return packet[:2] + struct.pack(">H", checksum(packet)) + packet[4:]


@dataclass
class Network:
    """The DHCP server's leases and what the AP saw, for the log."""
    leases: dict[bytes, bytes] = field(default_factory=dict)
    events: list[str] = field(default_factory=list)

    def lease_for(self, mac: bytes) -> bytes:
        if mac not in self.leases:
            self.leases[mac] = bytes([192, 168, 77, FIRST_LEASE + len(self.leases)])
        return self.leases[mac]

    def handle(self, eth: bytes) -> list[bytes]:
        """Answers to one Ethernet frame from a station."""
        if len(eth) < 14:
            return []
        src = eth[6:12]
        ethertype = struct.unpack(">H", eth[12:14])[0]
        payload = eth[14:]
        if ethertype == ETH_ARP:
            return self._arp(src, payload)
        if ethertype == ETH_IP and len(payload) >= 20 and payload[0] >> 4 == 4:
            ihl = (payload[0] & 0x0F) * 4
            proto, ip_src, ip_dst = payload[9], payload[12:16], payload[16:20]
            body = payload[ihl:struct.unpack(">H", payload[2:4])[0]]
            if proto == 17 and len(body) >= 8 and struct.unpack(">H", body[2:4])[0] == 67:
                return self._dhcp(src, body[8:])
            if proto == 1 and len(body) >= 8 and ip_dst == GATEWAY:
                return self._icmp(src, ip_src, body)
        return []

    def _arp(self, src: bytes, arp: bytes) -> list[bytes]:
        if len(arp) < 28 or struct.unpack(">H", arp[6:8])[0] != 1 or arp[24:28] != GATEWAY:
            return []
        sender_mac, sender_ip = arp[8:14], arp[14:18]
        self.events.append(f"AP:ARP:REPLY to={ip_text(sender_ip)}")
        reply = struct.pack(">HHBBH", 1, ETH_IP, 6, 4, 2) + GATEWAY_MAC + GATEWAY + sender_mac + sender_ip
        return [ethernet(src, GATEWAY_MAC, ETH_ARP, reply)]

    def _icmp(self, src: bytes, ip_src: bytes, body: bytes) -> list[bytes]:
        kind, _, _, ident, seq = struct.unpack(">BBHHH", body[:8])
        if kind == 8:
            self.events.append(f"AP:ICMP:ECHO:REQUEST from={ip_text(ip_src)} seq={seq}")
            reply = icmp_echo(0, ident, seq, body[8:])
            return [ethernet(src, GATEWAY_MAC, ETH_IP, ipv4(GATEWAY, ip_src, 1, reply))]
        if kind == 0:
            self.events.append(f"AP:ICMP:ECHO:REPLY from={ip_text(ip_src)} seq={seq}")
        return []

    def ping(self, mac: bytes, seq: int) -> bytes | None:
        """An echo request from the gateway to the station's lease."""
        ip = self.leases.get(mac)
        if ip is None:
            return None
        packet = icmp_echo(8, 0x4C5A, seq, b"lazyos-wifisim")
        return ethernet(mac, GATEWAY_MAC, ETH_IP, ipv4(GATEWAY, ip, 1, packet, ident=seq))

    def _dhcp(self, src: bytes, msg: bytes) -> list[bytes]:
        if len(msg) < 240 or msg[0] != 1 or msg[236:240] != DHCP_MAGIC:
            return []
        xid, flags, chaddr = msg[4:8], msg[10:12], msg[28:34]
        options = _options(msg[240:])
        kind = options.get(53, b"\0")[0]
        ip = self.lease_for(chaddr)
        if kind == 1:
            reply_kind, name = 2, "OFFER"
            self.events.append(f"AP:DHCP:DISCOVER mac={chaddr.hex()}")
        elif kind == 3:
            wanted = options.get(50, msg[12:16])
            if wanted != ip:
                self.events.append(f"AP:DHCP:NAK wanted={ip_text(wanted)}")
                return []
            reply_kind, name = 5, "ACK"
            self.events.append(f"AP:DHCP:REQUEST ip={ip_text(ip)}")
        else:
            return []
        reply = bytes([2, 1, 6, 0]) + xid + b"\0\0" + flags + bytes(4) + ip + GATEWAY + bytes(4)
        reply += chaddr + bytes(10) + bytes(192) + DHCP_MAGIC
        reply += bytes([53, 1, reply_kind, 54, 4]) + GATEWAY
        reply += bytes([51, 4]) + struct.pack(">I", LEASE_SECONDS)
        reply += bytes([1, 4]) + NETMASK + bytes([3, 4]) + GATEWAY + bytes([6, 4]) + GATEWAY + b"\xff"
        self.events.append(f"AP:DHCP:{name} ip={ip_text(ip)}")
        # Broadcast: the station has no address yet.
        packet = ipv4(GATEWAY, b"\xff" * 4, 17, udp(67, 68, reply))
        return [ethernet(BROADCAST_MAC, GATEWAY_MAC, ETH_IP, packet)]


def _options(data: bytes) -> dict[int, bytes]:
    out: dict[int, bytes] = {}
    at = 0
    while at < len(data):
        code = data[at]
        if code == 255:
            break
        if code == 0:
            at += 1
            continue
        if at + 2 > len(data):
            break
        length = data[at + 1]
        out[code] = data[at + 2:at + 2 + length]
        at += 2 + length
    return out
