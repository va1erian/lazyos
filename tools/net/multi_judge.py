"""Judging a multi-NIC boot from the packet captures (WP1,
docs/wifi-prerequisites-plan.md section 3.1).

`multi_run.py` boots the stack with two cards, each on its own user network
with its own capture. Nothing here reads a serial marker: every verdict is a
question about frames one card sent or received in a time window (the
captures carry the host's clock, so the harness marks the phases with it).
Pure functions over `pcap.Frame` lists, tested by `test_multi_judge.py`.
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pcap  # noqa: E402

DHCP_DISCOVER, DHCP_OFFER, DHCP_REQUEST, DHCP_ACK = 1, 2, 3, 5
FOREVER = float("inf")


def window(frames: list[pcap.Frame], since: float = 0.0, until: float = FOREVER) -> list[pcap.Frame]:
    """The frames captured in `[since, until]` (host seconds)."""
    return [f for f in frames if since <= f.time <= until]


def clock_offset(frames: list[pcap.Frame], mac: bytes, dst: str, host_mark: float) -> float | None:
    """How far this capture's clock is from the host's, from the first echo
    request the card `mac` sent to `dst` after the host noted `host_mark`
    (the harness types the ping at that moment). QEMU stamps captures with
    its own clock, which is not always the host's UTC (it was an hour off on
    a Windows host), so frames are never compared with host time directly.
    The error is the time the typed command took to reach the guest."""
    target = pcap.parse_ip(dst)
    for frame in frames:
        packet = pcap.parse_ipv4(frame.data)
        if packet is None or packet.eth_src != mac or packet.dst != target:
            continue
        icmp = pcap.parse_icmp_echo(packet)
        if icmp is not None and icmp.type == 8:
            return frame.time - host_mark
    return None


def dhcp_kinds(frames: list[pcap.Frame], mac: bytes) -> list[tuple[int, int, bool]]:
    """`(xid, message type, sent by the guest)` of the DHCP messages that
    belong to the client `mac`, in capture order."""
    found = []
    for frame in frames:
        packet = pcap.parse_ipv4(frame.data)
        dhcp = pcap.parse_dhcp(packet) if packet else None
        if dhcp is None or dhcp.client_mac != mac or dhcp.message_type is None:
            continue
        found.append((dhcp.xid, dhcp.message_type, packet.eth_src == mac))
    return found


def dhcp_cycles(frames: list[pcap.Frame], mac: bytes) -> int:
    """Complete DISCOVER, OFFER, REQUEST, ACK exchanges (one transaction id
    carrying all four, the guest sending the first and third)."""
    by_xid: dict[int, set[tuple[int, bool]]] = {}
    for xid, kind, from_guest in dhcp_kinds(frames, mac):
        by_xid.setdefault(xid, set()).add((kind, from_guest))
    want = {(DHCP_DISCOVER, True), (DHCP_OFFER, False), (DHCP_REQUEST, True), (DHCP_ACK, False)}
    return sum(1 for seen in by_xid.values() if want <= seen)


def discovers(frames: list[pcap.Frame], mac: bytes) -> int:
    """DISCOVERs the guest card sent."""
    return sum(1 for _, kind, from_guest in dhcp_kinds(frames, mac) if kind == DHCP_DISCOVER and from_guest)


def echo_requests(frames: list[pcap.Frame], mac: bytes, dst: str) -> int:
    """ICMP echo requests to `dst` that the card `mac` sent."""
    target = pcap.parse_ip(dst)
    count = 0
    for frame in frames:
        packet = pcap.parse_ipv4(frame.data)
        if packet is None or packet.eth_src != mac or packet.dst != target:
            continue
        icmp = pcap.parse_icmp_echo(packet)
        if icmp is not None and icmp.type == 8:
            count += 1
    return count


def udp_queries(frames: list[pcap.Frame], mac: bytes, server: str, port: int = 53) -> int:
    """UDP datagrams to `server:port` that the card `mac` sent (DNS queries)."""
    target = pcap.parse_ip(server)
    count = 0
    for frame in frames:
        packet = pcap.parse_ipv4(frame.data)
        if packet is None or packet.eth_src != mac or packet.dst != target or packet.proto != 17:
            continue
        if len(packet.payload) >= 8 and struct.unpack_from(">H", packet.payload, 2)[0] == port:
            count += 1
    return count


def frames_from(frames: list[pcap.Frame], mac: bytes) -> int:
    """Frames the card `mac` sent at all."""
    return sum(1 for f in frames if f.data[6:12] == mac)


def expect(problems: list[str], ok: bool, what: str) -> None:
    """Record `what` as a problem unless `ok`."""
    if not ok:
        problems.append(what)


def phase_traffic(
    name: str,
    cards: dict[str, tuple[list[pcap.Frame], bytes]],
    winner: str,
    *,
    target: str,
    resolver: dict[str, str],
    since: float,
    until: float = FOREVER,
) -> list[str]:
    """Traffic to the off-link `target` and DNS queries in a window must all
    leave by `winner` and none by the other cards. `cards` maps a card name to
    its capture and MAC, `resolver` a card name to the DNS server address its
    network offers."""
    problems: list[str] = []
    for card, (frames, mac) in cards.items():
        inside = window(frames, since, until)
        pings = echo_requests(inside, mac, target)
        queries = udp_queries(inside, mac, resolver[card])
        if card == winner:
            expect(problems, pings >= 1, f"{name}: no echo request to {target} left by {card}")
            expect(problems, queries >= 1, f"{name}: no DNS query to {resolver[card]} left by {card}")
        else:
            expect(problems, pings == 0, f"{name}: {pings} echo request(s) to {target} left by {card}, not {winner}")
            expect(problems, queries == 0, f"{name}: {queries} DNS query(ies) left by {card}, not {winner}")
    return problems


def phase_dhcp(name: str, frames: list[pcap.Frame], mac: bytes, since: float, until: float = FOREVER,
               *, restarted: bool = False) -> list[str]:
    """A complete DHCP exchange in the window; with `restarted`, a DISCOVER
    too (the interface started over, not renewed)."""
    inside = window(frames, since, until)
    problems: list[str] = []
    expect(problems, dhcp_cycles(inside, mac) >= 1, f"{name}: no complete DISCOVER/OFFER/REQUEST/ACK exchange")
    if restarted:
        expect(problems, discovers(inside, mac) >= 1, f"{name}: no DISCOVER after the link came back")
    return problems
