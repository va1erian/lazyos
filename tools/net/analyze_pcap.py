#!/usr/bin/env python3
"""Judge a packet capture of LazyOS talking to QEMU's user-mode network.

The proof that the network driver works is not a log line but what crossed the
wire: QEMU records the guest's NIC with `-object filter-dump`, and this script
checks that capture. The serial markers only tell the harness when the guest is
done; every verdict here comes from the packets (`tools/net/run.py`).

Checks (each is a function returning the reasons it failed, so the tests can
show that every one fails when it should):

  * ARP exchange: a broadcast request from the guest for the gateway, and its
    reply (from the gateway, to the guest, sender fields consistent with the
    Ethernet header), *after* it, at least ``--min-arp-pairs`` times;
  * frame-length policy: no captured frame is shorter than an Ethernet header
    (14 bytes) or longer than MTU + 14 (1514), so the driver's dropped frames
    never reached the wire;
  * DHCP (``--min-dhcp``): complete DISCOVER/OFFER/REQUEST/ACK exchanges in
    order with one transaction id, the ACK granting the OFFERed address;
  * IP sanity (with the DHCP or ping checks): every IPv4 frame the guest sends
    has a correct header checksum and length, every ICMP message a correct
    checksum;
  * ICMP echo (``--min-pings``): requests to the gateway answered, in order, by a
    reply with the same identifier, sequence number and payload; no echo request
    to an address that cannot be a host reached the wire;
  * probe frames (``--expect-probe``): the frames the hostile-input probe sent
    at the legal extremes are on the wire exactly - 14 and 1514 bytes, payload
    intact - and the ones one byte outside them (13 and 1515) are not.

    python tools/net/analyze_pcap.py shots/net/net.pcap --min-arp-pairs 42 --expect-probe

Exit status is non-zero on any failure, including a missing, empty or
truncated capture.
"""

from __future__ import annotations

import argparse
import struct
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pcap  # noqa: E402
from pcap import Frame, PcapError, mac_text, parse_ip, parse_mac  # noqa: E402

DEFAULT_GUEST_MAC = "52:54:00:12:34:56"
DEFAULT_GATEWAY = "10.0.2.2"
MIN_FRAME = 14
MAX_FRAME = 1514
#: EtherType of the probe frames (`user/src/bin/nicctl/probe.rs`).
PROBE_ETHERTYPE = 0x88B5
#: The lengths the probe sends at the legal extremes, and the ones just outside.
PROBE_LEGAL = (14, 1514)
PROBE_ILLEGAL = (13, 1515)


@dataclass
class Report:
    """Named verdicts, printed in order; ``ok`` is false if any failed."""

    lines: list[str]
    ok: bool = True

    def passed(self, name: str, detail: str = "") -> None:
        self.lines.append(f"NET:PCAP:{name}:PASS" + (f" {detail}" if detail else ""))

    def failed(self, name: str, reasons: list[str]) -> None:
        self.ok = False
        for reason in reasons:
            self.lines.append(f"NET:PCAP:{name}:FAIL {reason}")


# ---- checks --------------------------------------------------------------------


def arp_exchanges(frames: list[Frame], guest_mac: bytes, gateway_ip: bytes) -> tuple[int, list[str]]:
    """Count request/reply pairs with the gateway and explain what is wrong.

    A request is a broadcast ARP request from the guest (Ethernet source and
    ARP sender both the guest's MAC) for `gateway_ip`. A reply answers it if it
    comes *later* in the capture, is sent to the guest, names the gateway as
    sender, and its Ethernet source equals its ARP sender MAC. Each reply
    answers one request.
    """
    problems: list[str] = []
    requests: list[Frame] = []
    replies: list[Frame] = []
    for frame in frames:
        arp = pcap.parse_arp(frame.data)
        if arp is None:
            continue
        if arp.op == 1 and arp.eth_src == guest_mac and arp.sender_mac == guest_mac and arp.target_ip == gateway_ip:
            if arp.eth_dst != b"\xff" * 6:
                problems.append(f"frame {frame.index}: the ARP request was not broadcast")
            else:
                requests.append(frame)
        elif arp.op == 2 and arp.sender_ip == gateway_ip:
            replies.append(frame)
    if not requests:
        problems.append("no ARP request from the guest for the gateway is in the capture")
    if not replies:
        problems.append("no ARP reply from the gateway is in the capture")
    if requests and replies and replies[0].index < requests[0].index:
        problems.append(f"the first reply (frame {replies[0].index}) comes before the first request (frame {requests[0].index})")

    pairs = 0
    unused = list(replies)
    for request in requests:
        match = next((r for r in unused if r.index > request.index), None)
        if match is None:
            problems.append(f"the request in frame {request.index} has no reply after it")
            continue
        arp = pcap.parse_arp(match.data)
        assert arp is not None
        if arp.eth_dst != guest_mac or arp.target_mac != guest_mac:
            problems.append(f"frame {match.index}: the reply is not addressed to the guest ({mac_text(arp.eth_dst)} / {mac_text(arp.target_mac)})")
        elif arp.eth_src != arp.sender_mac:
            problems.append(f"frame {match.index}: the reply's Ethernet source {mac_text(arp.eth_src)} differs from its ARP sender {mac_text(arp.sender_mac)}")
        else:
            pairs += 1
        unused.remove(match)
    return pairs, problems


def check_arp(frames: list[Frame], guest_mac: bytes, gateway_ip: bytes, min_pairs: int) -> tuple[int, list[str]]:
    pairs, problems = arp_exchanges(frames, guest_mac, gateway_ip)
    if pairs < min_pairs:
        problems.append(f"{pairs} complete ARP exchange(s) with the gateway, {min_pairs} required")
    return pairs, problems


def check_frame_sizes(frames: list[Frame], lo: int = MIN_FRAME, hi: int = MAX_FRAME) -> list[str]:
    """No frame on the wire may be outside the driver's length policy."""
    problems = []
    for frame in frames:
        if frame.length < lo:
            problems.append(f"frame {frame.index} is {frame.length} bytes, shorter than an Ethernet header ({lo})")
        elif frame.length > hi:
            problems.append(f"frame {frame.index} is {frame.length} bytes, longer than MTU + 14 ({hi})")
        if frame.truncated:
            problems.append(f"frame {frame.index} was captured truncated ({frame.length} of {frame.orig_len} bytes)")
    return problems


def probe_frames(frames: list[Frame], guest_mac: bytes) -> list[Frame]:
    return [f for f in frames if pcap.ethertype(f.data) == PROBE_ETHERTYPE and f.data[6:12] == guest_mac]


def check_probe(frames: list[Frame], guest_mac: bytes) -> tuple[list[int], list[str]]:
    """The probe's boundary frames: legal extremes present and intact, frames
    just outside them absent."""
    problems = []
    found = probe_frames(frames, guest_mac)
    lengths = sorted({f.length for f in found})
    for length in PROBE_LEGAL:
        matching = [f for f in found if f.length == length]
        if not matching:
            problems.append(f"no {length}-byte probe frame reached the wire (the legal extreme was dropped)")
        for frame in matching:
            expected = bytes([0xFF] * 6) + guest_mac + bytes([PROBE_ETHERTYPE >> 8, PROBE_ETHERTYPE & 0xFF])
            expected += bytes((i ^ 0x5A) & 0xFF for i in range(14, length))
            if frame.data != expected:
                first = next((i for i, (a, b) in enumerate(zip(frame.data, expected)) if a != b), None)
                problems.append(f"frame {frame.index}: the {length}-byte probe frame's payload differs at byte {first}")
    for length in PROBE_ILLEGAL:
        # A 13-byte frame is cut from the template before the EtherType is
        # complete, so look for it by length among frames from the guest too.
        leaked = [f for f in frames if f.length == length and f.data[6:12] == guest_mac]
        for frame in leaked:
            problems.append(f"frame {frame.index}: a {length}-byte frame reached the wire (it must be dropped)")
    extra = [n for n in lengths if n not in PROBE_LEGAL]
    if extra:
        problems.append(f"probe frames of unexpected lengths reached the wire: {extra}")
    return lengths, problems


# ---- DHCP, IP sanity, ICMP echo (stage N2) ----------------------------------------

DHCP_DISCOVER, DHCP_OFFER, DHCP_REQUEST, DHCP_ACK, DHCP_NAK = 1, 2, 3, 5, 6
DHCP_NAMES = {1: "DISCOVER", 2: "OFFER", 3: "REQUEST", 5: "ACK", 6: "NAK"}


def dhcp_messages(frames: list[Frame]) -> list[tuple[Frame, pcap.Dhcp]]:
    out = []
    for frame in frames:
        packet = pcap.parse_ipv4(frame.data)
        if packet is not None:
            message = pcap.parse_dhcp(packet)
            if message is not None:
                out.append((frame, message))
    return out


def check_dhcp(frames: list[Frame], guest_mac: bytes, min_exchanges: int) -> tuple[int, list[str]]:
    """Count complete DISCOVER/OFFER/REQUEST/ACK exchanges and explain what is wrong.

    An exchange is four messages in this order, sharing one transaction id: the
    guest's DISCOVER, a server's OFFER, the guest's REQUEST, a server's ACK
    that grants the address the OFFER named. Every client message carries the
    guest's hardware address; a NAK is a failure.
    """
    problems: list[str] = []
    messages = dhcp_messages(frames)
    by_xid: dict[int, list[tuple[Frame, pcap.Dhcp]]] = {}
    for frame, message in messages:
        if message.op == 1 and message.client_mac != guest_mac:
            problems.append(
                f"frame {frame.index}: a DHCP client message carries hardware address "
                f"{mac_text(message.client_mac)}, not the guest's"
            )
        if message.message_type == DHCP_NAK:
            problems.append(f"frame {frame.index}: the server sent a DHCPNAK")
        by_xid.setdefault(message.xid, []).append((frame, message))
    if not messages:
        problems.append("no DHCP message is in the capture")
    complete = 0
    for xid, group in by_xid.items():
        def first(kind, after=-1):
            return next(((f, m) for f, m in group if m.message_type == kind and f.index > after), None)

        discover = first(DHCP_DISCOVER)
        offer = first(DHCP_OFFER, discover[0].index) if discover else None
        request = first(DHCP_REQUEST, offer[0].index) if offer else None
        ack = first(DHCP_ACK, request[0].index) if request else None
        if discover and offer and request and ack:
            offered, granted = offer[1].yiaddr, ack[1].yiaddr
            if offered != granted or offered == bytes(4):
                problems.append(
                    f"transaction {xid:#010x}: the ACK grants {pcap.ip_text(granted)} "
                    f"but the OFFER named {pcap.ip_text(offered)}"
                )
            else:
                complete += 1
            continue
        names = ", ".join(f"{DHCP_NAMES.get(m.message_type, m.message_type)}@{f.index}" for f, m in group)
        if discover:
            missing = "OFFER" if not offer else "REQUEST" if not request else "ACK"
            problems.append(f"transaction {xid:#010x}: no {missing} in order after the earlier messages ({names})")
        else:
            problems.append(f"transaction {xid:#010x}: server messages without a DISCOVER first ({names})")
    if complete < min_exchanges:
        problems.append(f"{complete} complete DHCP exchange(s), {min_exchanges} required")
    return complete, problems


def check_ip_sanity(frames: list[Frame], guest_mac: bytes) -> list[str]:
    """Everything the guest sends as IPv4 is well formed: a correct header
    checksum, a total length that matches the frame, and, for ICMP, a correct
    ICMP checksum."""
    problems = []
    for frame in frames:
        if frame.data[6:12] != guest_mac or pcap.ethertype(frame.data) != pcap.ETHERTYPE_IPV4:
            continue
        packet = pcap.parse_ipv4(frame.data)
        if packet is None:
            problems.append(f"frame {frame.index}: an IPv4 frame from the guest does not parse")
            continue
        if not packet.header_ok:
            problems.append(f"frame {frame.index}: the IPv4 header checksum is wrong")
        total = struct.unpack_from(">H", frame.data, 16)[0]
        # Ethernet may pad a short frame to 60 bytes; anything else is a mismatch.
        if total != len(frame.data) - 14 and not (len(frame.data) == 60 and total < 46):
            problems.append(
                f"frame {frame.index}: the IPv4 total length {total} does not match the frame ({len(frame.data) - 14})"
            )
        echo = pcap.parse_icmp_echo(packet)
        if echo is not None and not echo.checksum_ok:
            problems.append(f"frame {frame.index}: the ICMP checksum is wrong")
    return problems


def echo_messages(frames: list[Frame]) -> list[tuple[Frame, pcap.Ipv4, pcap.Icmp]]:
    out = []
    for frame in frames:
        packet = pcap.parse_ipv4(frame.data)
        echo = pcap.parse_icmp_echo(packet) if packet else None
        if packet and echo:
            out.append((frame, packet, echo))
    return out


def invalid_destination(ip: bytes) -> bool:
    return ip[0] in (0, 127) or ip[0] >= 224


def check_ping(frames: list[Frame], guest_mac: bytes, gateway_ip: bytes, min_pairs: int) -> tuple[int, list[str]]:
    """Count echo request/reply pairs with the gateway and explain what is wrong.

    A request is an ICMP echo request (type 8, code 0) sent by the guest to the
    gateway; its reply is the *next unmatched* echo reply (type 0) from the
    gateway with the same identifier and sequence number, later in the
    capture, carrying byte-for-byte the same payload and correct checksums. An
    echo request from the guest to an address that cannot be a host
    (unspecified, loopback, multicast, broadcast) is a failure: the stack must
    refuse those before anything reaches the wire.
    """
    problems: list[str] = []
    requests = []
    replies = []
    for frame, packet, echo in echo_messages(frames):
        if echo.type == 8 and packet.eth_src == guest_mac:
            if invalid_destination(packet.dst):
                problems.append(f"frame {frame.index}: an echo request to {pcap.ip_text(packet.dst)} reached the wire")
            elif packet.dst == gateway_ip:
                if echo.code != 0:
                    problems.append(f"frame {frame.index}: the echo request has code {echo.code}")
                requests.append((frame, packet, echo))
        elif echo.type == 0 and packet.src == gateway_ip and packet.eth_dst == guest_mac:
            replies.append((frame, packet, echo))
    if not requests:
        problems.append("no ICMP echo request to the gateway is in the capture")
    if not replies:
        problems.append("no ICMP echo reply from the gateway is in the capture")
    if requests and replies and replies[0][0].index < requests[0][0].index:
        problems.append(
            f"the first reply (frame {replies[0][0].index}) comes before "
            f"the first request (frame {requests[0][0].index})"
        )
    pairs = 0
    unused = list(replies)
    for frame, _packet, echo in requests:
        match = next(
            (r for r in unused if r[0].index > frame.index and r[2].ident == echo.ident and r[2].seq == echo.seq),
            None,
        )
        if match is None:
            problems.append(f"the echo request in frame {frame.index} (seq {echo.seq}) has no reply after it")
            continue
        unused.remove(match)
        reply_frame, reply_packet, reply = match
        if reply.data != echo.data:
            problems.append(
                f"frame {reply_frame.index}: the reply's payload differs from the request's "
                f"(frame {frame.index}, seq {echo.seq})"
            )
        elif not reply.checksum_ok or not reply_packet.header_ok:
            problems.append(f"frame {reply_frame.index}: the reply's checksum is wrong")
        else:
            pairs += 1
    if pairs < min_pairs:
        problems.append(f"{pairs} complete echo exchange(s) with the gateway, {min_pairs} required")
    return pairs, problems


# ---- driver ----------------------------------------------------------------------


def analyze(frames: list[Frame], *, guest_mac: bytes, gateway_ip: bytes, min_arp_pairs: int, expect_probe: bool,
            min_frames: int = 1, min_dhcp: int = 0, min_pings: int = 0) -> Report:
    report = Report([])
    tx = sum(1 for f in frames if f.data[6:12] == guest_mac)
    report.lines.append(f"NET:PCAP:FRAMES total={len(frames)} from_guest={tx} to_guest={len(frames) - tx}")
    if len(frames) < min_frames:
        report.failed("FRAMES", [f"{len(frames)} frames captured, {min_frames} required"])
        return report
    if min_arp_pairs:
        pairs, problems = check_arp(frames, guest_mac, gateway_ip, min_arp_pairs)
        if problems:
            report.failed("ARP", problems)
        else:
            report.passed("ARP", f"pairs={pairs} guest={mac_text(guest_mac)} gateway={pcap.ip_text(gateway_ip)}")
    problems = check_frame_sizes(frames)
    if problems:
        report.failed("POLICY", problems[:10])
    else:
        report.passed("POLICY", f"every frame is {MIN_FRAME}..{MAX_FRAME} bytes")
    if min_dhcp:
        complete, problems = check_dhcp(frames, guest_mac, min_dhcp)
        if problems:
            report.failed("DHCP", problems[:10])
        else:
            report.passed("DHCP", f"exchanges={complete}")
    if min_dhcp or min_pings:
        problems = check_ip_sanity(frames, guest_mac)
        if problems:
            report.failed("IP", problems[:10])
        else:
            report.passed("IP", "every IPv4 frame from the guest is well formed")
    if min_pings:
        pairs, problems = check_ping(frames, guest_mac, gateway_ip, min_pings)
        if problems:
            report.failed("PING", problems[:10])
        else:
            report.passed("PING", f"pairs={pairs} gateway={pcap.ip_text(gateway_ip)}")
    if expect_probe:
        lengths, problems = check_probe(frames, guest_mac)
        if problems:
            report.failed("PROBE", problems[:10])
        else:
            report.passed("PROBE", f"probe frame lengths on the wire={lengths}")
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("pcap", help="the capture QEMU's filter-dump wrote")
    parser.add_argument("--guest-mac", default=DEFAULT_GUEST_MAC)
    parser.add_argument("--gateway", default=DEFAULT_GATEWAY)
    parser.add_argument("--min-arp-pairs", type=int, default=1, help="complete ARP request/reply pairs required")
    parser.add_argument("--expect-probe", action="store_true", help="check the hostile-input probe's boundary frames")
    parser.add_argument("--min-frames", type=int, default=1)
    parser.add_argument("--min-dhcp", type=int, default=0, help="complete DHCP exchanges required (stage N2)")
    parser.add_argument("--min-pings", type=int, default=0, help="ICMP echo pairs with the gateway required (stage N2)")
    args = parser.parse_args(argv)
    try:
        frames = pcap.read_pcap(args.pcap)
    except (PcapError, OSError) as exc:
        print(f"NET:PCAP:FAIL {exc}")
        return 1
    report = analyze(
        frames,
        guest_mac=parse_mac(args.guest_mac),
        gateway_ip=parse_ip(args.gateway),
        min_arp_pairs=args.min_arp_pairs,
        expect_probe=args.expect_probe,
        min_frames=args.min_frames,
        min_dhcp=args.min_dhcp,
        min_pings=args.min_pings,
    )
    print("\n".join(report.lines))
    return 0 if report.ok else 1


if __name__ == "__main__":
    sys.exit(main())
