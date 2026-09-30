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
  * probe frames (``--expect-probe``): the frames the hostile-input probe sent
    at the legal extremes are on the wire exactly - 14 and 1514 bytes, payload
    intact - and the ones one byte outside them (13 and 1515) are not.

    python tools/net/analyze_pcap.py shots/net/net.pcap --min-arp-pairs 42 --expect-probe

Exit status is non-zero on any failure, including a missing, empty or
truncated capture.
"""

from __future__ import annotations

import argparse
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


# ---- driver ----------------------------------------------------------------------


def analyze(frames: list[Frame], *, guest_mac: bytes, gateway_ip: bytes, min_arp_pairs: int, expect_probe: bool, min_frames: int = 1) -> Report:
    report = Report([])
    tx = sum(1 for f in frames if f.data[6:12] == guest_mac)
    report.lines.append(f"NET:PCAP:FRAMES total={len(frames)} from_guest={tx} to_guest={len(frames) - tx}")
    if len(frames) < min_frames:
        report.failed("FRAMES", [f"{len(frames)} frames captured, {min_frames} required"])
        return report
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
    )
    print("\n".join(report.lines))
    return 0 if report.ok else 1


if __name__ == "__main__":
    sys.exit(main())
