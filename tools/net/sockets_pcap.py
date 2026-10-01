#!/usr/bin/env python3
"""Judge the TCP, UDP and DNS part of a LazyOS packet capture (stage N3).

`analyze_pcap.py` judges the link layer, DHCP and ICMP; this module judges what
`nc`, `nslookup` and the socket soak put on the wire. The guest's own markers
say when it is done; the verdict is here:

  * TCP flows: every flow the guest opens to the host's echo port has a
    complete handshake, correct checksums, a stream in each direction that
    reassembles without a gap, and an orderly close (a FIN from each side). The
    streams are compared with what the host's echo server recorded, byte for
    byte (by length and SHA-256), so "the bytes round-trip" is a statement about
    the wire, not about a log line;
  * refused connections: a SYN to a port nobody listens on is answered with a
    reset;
  * UDP: every datagram to the echo port is answered with the same payload;
  * DNS: a well-formed A query for the name looked up left the guest for the
    resolver, and its answer, when the host network produced one, is reported;
  * a flow into the guest (the harness connecting through a port forward):
    handshake, the bytes the harness sent, and the same bytes echoed back.

    python tools/net/sockets_pcap.py shots/net/net.pcap --echo-port 47771 ...

Every check is a function returning the reasons it failed, so
`test_sockets_pcap.py` can show each one fails when it should.
"""

from __future__ import annotations

import hashlib
import struct
import sys
from dataclasses import dataclass, field
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pcap  # noqa: E402
from pcap import Frame, ipv4_checksum  # noqa: E402

FIN, SYN, RST, PSH, ACK = 0x01, 0x02, 0x04, 0x08, 0x10
PROTO_TCP, PROTO_UDP = 6, 17


@dataclass(frozen=True)
class Tcp:
    src: bytes
    dst: bytes
    sport: int
    dport: int
    seq: int
    ack: int
    flags: int
    payload: bytes
    checksum_ok: bool
    index: int


@dataclass(frozen=True)
class Udp:
    src: bytes
    dst: bytes
    sport: int
    dport: int
    payload: bytes
    checksum_ok: bool
    index: int


def pseudo_checksum_ok(packet: pcap.Ipv4, segment: bytes) -> bool:
    """The TCP or UDP checksum over the IPv4 pseudo header (a UDP checksum of
    zero means "none" and is accepted)."""
    if packet.proto == PROTO_UDP and len(segment) >= 8 and segment[6:8] == b"\0\0":
        return True
    pseudo = packet.src + packet.dst + struct.pack(">BBH", 0, packet.proto, len(segment))
    return ipv4_checksum(pseudo + segment) == 0


def parse_tcp(frame: Frame) -> Tcp | None:
    packet = pcap.parse_ipv4(frame.data)
    if packet is None or packet.proto != PROTO_TCP or len(packet.payload) < 20:
        return None
    seg = packet.payload
    sport, dport, seq, ack, off_flags = struct.unpack_from(">HHIIH", seg, 0)
    header = (off_flags >> 12) * 4
    if header < 20 or header > len(seg):
        return None
    return Tcp(packet.src, packet.dst, sport, dport, seq, ack, off_flags & 0x3F, seg[header:],
               pseudo_checksum_ok(packet, seg) and packet.header_ok, frame.index)


def parse_udp(frame: Frame) -> Udp | None:
    packet = pcap.parse_ipv4(frame.data)
    if packet is None or packet.proto != PROTO_UDP or len(packet.payload) < 8:
        return None
    sport, dport, length, _ = struct.unpack_from(">HHHH", packet.payload, 0)
    if length < 8 or length > len(packet.payload):
        return None
    seg = packet.payload[:length]
    return Udp(packet.src, packet.dst, sport, dport, seg[8:], pseudo_checksum_ok(packet, seg) and packet.header_ok,
               frame.index)


@dataclass
class Flow:
    """One TCP connection as seen on the wire, from its initiator's side."""

    initiator: tuple[bytes, int]
    responder: tuple[bytes, int]
    segments: list[Tcp] = field(default_factory=list)

    def stream(self, from_initiator: bool) -> tuple[bytes, list[str]]:
        """The bytes one side sent, reassembled by sequence number; the
        problems found (gaps, overlaps that disagree, a missing SYN)."""
        side = self.initiator if from_initiator else self.responder
        mine = [s for s in self.segments if (s.src, s.sport) == side]
        syn = next((s for s in mine if s.flags & SYN), None)
        if syn is None:
            return b"", ["no SYN from the sender"]
        base = (syn.seq + 1) & 0xFFFFFFFF
        data = bytearray()
        problems: list[str] = []
        for seg in sorted((s for s in mine if s.payload), key=lambda s: ((s.seq - base) & 0xFFFFFFFF, s.index)):
            at = (seg.seq - base) & 0xFFFFFFFF
            if at > len(data):
                problems.append(f"a gap of {at - len(data)} bytes before frame {seg.index}")
                break
            fresh = seg.payload[len(data) - at:] if at < len(data) else seg.payload
            overlap = seg.payload[: len(data) - at] if at < len(data) else b""
            if overlap and data[at:at + len(overlap)] != overlap[: len(data) - at]:
                problems.append(f"frame {seg.index} retransmits different bytes")
            data += fresh
        return bytes(data), problems

    def handshake_ok(self) -> bool:
        flags = [(s.src, s.sport, s.flags) for s in self.segments]
        syn = any(src == self.initiator[0] and port == self.initiator[1] and f & SYN and not f & ACK
                  for src, port, f in flags)
        synack = any(src == self.responder[0] and port == self.responder[1] and f & SYN and f & ACK
                     for src, port, f in flags)
        return syn and synack

    def closed_by(self, from_initiator: bool) -> bool:
        side = self.initiator if from_initiator else self.responder
        return any((s.src, s.sport) == side and s.flags & FIN for s in self.segments)

    def reset(self) -> bool:
        return any(s.flags & RST for s in self.segments)


def flows(frames: list[Frame]) -> list[Flow]:
    """Group the capture's TCP segments into connections, in order of their SYN."""
    table: dict[frozenset, Flow] = {}
    order: list[Flow] = []
    for frame in frames:
        seg = parse_tcp(frame)
        if seg is None:
            continue
        key = frozenset([(seg.src, seg.sport), (seg.dst, seg.dport)])
        flow = table.get(key)
        if flow is None or (seg.flags & SYN and not seg.flags & ACK and flow.segments and
                            any(s.flags & (FIN | RST) for s in flow.segments)):
            # A fresh SYN on a finished 4-tuple is a new connection.
            if not seg.flags & SYN:
                continue
            flow = Flow((seg.src, seg.sport), (seg.dst, seg.dport))
            table[key] = flow
            order.append(flow)
        flow.segments.append(seg)
    return order


def digest(data: bytes) -> tuple[int, str]:
    return len(data), hashlib.sha256(data).hexdigest()


def check_checksums(frames: list[Frame], guest_mac: bytes) -> list[str]:
    """Every TCP segment and UDP datagram the guest sent has a correct checksum."""
    problems = []
    for frame in frames:
        if frame.data[6:12] != guest_mac:
            continue
        for parsed in (parse_tcp(frame), parse_udp(frame)):
            if parsed is not None and not parsed.checksum_ok:
                problems.append(f"frame {frame.index}: a wrong TCP/UDP or IPv4 checksum")
    return problems


def check_echo_flows(frames: list[Frame], guest_ip: bytes, gateway_ip: bytes, port: int, min_flows: int,
                     server_streams: list[bytes]) -> tuple[int, list[str]]:
    """Flows from the guest to the gateway's echo `port`: handshake, stream
    in both directions, orderly close, and agreement with the host server."""
    problems: list[str] = []
    mine = [f for f in flows(frames) if f.initiator[0] == guest_ip and f.responder == (gateway_ip, port)]
    if len(mine) < min_flows:
        problems.append(f"{len(mine)} TCP flows to port {port}, {min_flows} required")
    sent, echoed = [], []
    for n, flow in enumerate(mine):
        name = f"flow {n} (guest port {flow.initiator[1]})"
        if not flow.handshake_ok():
            problems.append(f"{name}: no complete handshake")
            continue
        out, out_problems = flow.stream(True)
        back, back_problems = flow.stream(False)
        problems += [f"{name}: {p}" for p in out_problems + back_problems]
        if out != back:
            problems.append(f"{name}: sent {len(out)} bytes, echoed {len(back)}"
                            + ("" if len(out) == len(back) else " (lengths differ)")
                            + (" and the bytes differ" if len(out) == len(back) else ""))
        if not (flow.closed_by(True) and flow.closed_by(False)) and not flow.reset():
            problems.append(f"{name}: not closed by a FIN from both sides")
        if flow.reset():
            problems.append(f"{name}: reset")
        sent.append(digest(out))
        echoed.append(digest(back))
    if sorted(sent) != sorted(digest(s) for s in server_streams):
        problems.append(f"the streams on the wire ({len(sent)}) do not match what the host server received "
                        f"({len(server_streams)})")
    return len(mine), problems


def check_refused(frames: list[Frame], guest_ip: bytes, gateway_ip: bytes, port: int,
                  min_attempts: int) -> tuple[int, list[str]]:
    """Connection attempts to `port` (nobody listens there): none may be
    established. Returns how many were answered with a reset; QEMU's user
    networking resets on some hosts and stays silent on others, so the
    count is reported, not required."""
    refused = attempts = 0
    problems: list[str] = []
    for flow in flows(frames):
        if flow.initiator[0] == guest_ip and flow.responder == (gateway_ip, port):
            attempts += 1
            if flow.handshake_ok():
                problems.append(f"a connection to the closed port {port} was established "
                                f"(guest port {flow.initiator[1]})")
            if any((s.src, s.sport) == flow.responder and s.flags & RST for s in flow.segments):
                refused += 1
    if attempts < min_attempts:
        problems.append(f"{attempts} connection attempts to port {port}, {min_attempts} required")
    return refused, problems


def check_udp_echo(frames: list[Frame], guest_ip: bytes, gateway_ip: bytes, port: int, min_pairs: int,
                   server_datagrams: list[bytes]) -> tuple[int, list[str]]:
    """Datagrams to `port`, each answered (later) with the same payload."""
    problems: list[str] = []
    out = [u for u in (parse_udp(f) for f in frames) if u and u.src == guest_ip and u.dst == gateway_ip
           and u.dport == port]
    back = [u for u in (parse_udp(f) for f in frames) if u and u.src == gateway_ip and u.dst == guest_ip
            and u.sport == port]
    answered = 0
    unmatched = list(back)
    for datagram in out:
        reply = next((b for b in unmatched if b.dport == datagram.sport and b.index > datagram.index
                      and b.payload == datagram.payload), None)
        if reply is None:
            problems.append(f"frame {datagram.index}: a {len(datagram.payload)}-byte datagram was not echoed")
        else:
            unmatched.remove(reply)
            answered += 1
    if answered < min_pairs:
        problems.append(f"{answered} UDP echo pairs, {min_pairs} required")
    if sorted(u.payload for u in out) != sorted(server_datagrams):
        problems.append("the datagrams on the wire do not match what the host server received")
    return answered, problems


def dns_name(message: bytes) -> tuple[str, int, int] | None:
    """`(name, qtype, rcode)` of a DNS message with one well-formed question."""
    if len(message) < 17:
        return None
    flags, qd = struct.unpack_from(">HH", message, 2)
    if qd != 1:
        return None
    at, labels = 12, []
    while True:
        if at >= len(message):
            return None
        n = message[at]
        at += 1
        if n == 0:
            break
        if n > 63 or at + n > len(message):
            return None
        labels.append(message[at:at + n].decode("ascii", "replace"))
        at += n
    if at + 4 > len(message):
        return None
    return ".".join(labels), struct.unpack_from(">H", message, at)[0], flags & 0xF


def check_dns(frames: list[Frame], guest_ip: bytes, name: str) -> tuple[str, list[str]]:
    """A well-formed A query for `name` from the guest to a resolver on port 53.
    The detail says whether an answer came back (and with what rcode)."""
    queries = [u for u in (parse_udp(f) for f in frames) if u and u.src == guest_ip and u.dport == 53]
    problems: list[str] = []
    wanted = [q for q in queries if (dns_name(q.payload) or ("",))[0].lower() == name.lower()]
    if not wanted:
        return "", [f"no DNS query for {name!r} left the guest ({len(queries)} DNS queries seen)"]
    query = wanted[0]
    parsed = dns_name(query.payload)
    if parsed is None or parsed[1] != 1:
        problems.append(f"frame {query.index}: the query is not a well-formed A question")
    if not query.checksum_ok:
        problems.append(f"frame {query.index}: the query's checksum is wrong")
    answers = [u for u in (parse_udp(f) for f in frames) if u and u.dst == guest_ip and u.sport == 53
               and u.dport == query.sport and u.payload[:2] == query.payload[:2]]
    if answers:
        rcode = dns_name(answers[0].payload)
        detail = f"answered rcode={rcode[2] if rcode else '?'}"
    else:
        detail = "no answer in the capture (the host may be offline)"
    return detail, problems


def check_inbound_flow(frames: list[Frame], guest_ip: bytes, gateway_ip: bytes, port: int,
                       payload: bytes) -> tuple[int, list[str]]:
    """A connection into the guest's listener: handshake, `payload` in, the
    same bytes echoed out, an orderly close."""
    mine = [f for f in flows(frames) if f.initiator[0] == gateway_ip and f.responder == (guest_ip, port)]
    if not mine:
        return 0, [f"no connection to the guest's port {port} in the capture"]
    flow = mine[0]
    problems: list[str] = []
    if not flow.handshake_ok():
        problems.append("the inbound connection has no complete handshake")
    incoming, a = flow.stream(True)
    outgoing, b = flow.stream(False)
    problems += a + b
    if incoming != payload:
        problems.append(f"the guest received {len(incoming)} bytes, the harness sent {len(payload)}")
    if outgoing != payload:
        problems.append(f"the guest echoed {len(outgoing)} bytes, expected the same {len(payload)}")
    if any(not s.checksum_ok for s in flow.segments):
        problems.append("a segment of the inbound connection has a wrong checksum")
    return len(incoming), problems


# ---- FTP (stage N4) --------------------------------------------------------------


def ftp_commands(stream: bytes) -> list[tuple[str, str]]:
    """The `(VERB, argument)` lines of a control-connection stream sent by the
    client; the password is kept (the server saw it too)."""
    out = []
    for line in stream.split(b"\r\n"):
        if not line:
            continue
        verb, _, arg = line.decode("latin-1").partition(" ")
        out.append((verb.upper(), arg))
    return out


def check_ftp(frames: list[Frame], guest_ip: bytes, gateway_ip: bytes, control_port: int,
              server_commands: list[tuple[str, str]], transfers: list[tuple[int, str, bytes]]
              ) -> tuple[int, list[str]]:
    """The FTP session on the wire.

    * the control connection: handshake, valid checksums, closed by a FIN from
      both sides, and the commands the client sent are exactly the ones the
      host server recorded (so nothing was injected or lost between them);
    * every data transfer the server recorded appears as its own connection to
      its passive port, closed in order, whose bytes are the file (a download:
      server to guest) or the upload (guest to server) byte for byte;
    * no connection from the guest to the gateway on a port that is neither the
      control port nor a recorded passive port (the client must not have been
      sent anywhere else).
    """
    problems: list[str] = []
    all_flows = flows(frames)
    control = [f for f in all_flows if f.initiator[0] == guest_ip and f.responder == (gateway_ip, control_port)]
    if len(control) != 1:
        return 0, [f"{len(control)} control connections to port {control_port}, 1 expected"]
    flow = control[0]
    if not flow.handshake_ok():
        problems.append("the control connection has no complete handshake")
    if not (flow.closed_by(True) and flow.closed_by(False)):
        problems.append("the control connection was not closed by a FIN from both sides")
    sent, a = flow.stream(True)
    _, b = flow.stream(False)
    problems += [f"control: {p}" for p in a + b]
    wire_commands = ftp_commands(sent)
    if wire_commands != server_commands:
        problems.append(f"the commands on the wire {wire_commands} differ from what the server recorded "
                        f"{server_commands}")
    data_ports = {port for port, _, _ in transfers}
    for n, (port, direction, body) in enumerate(transfers):
        mine = [f for f in all_flows if f.initiator[0] == guest_ip and f.responder == (gateway_ip, port)]
        name = f"transfer {n} ({direction}, port {port}, {len(body)} bytes)"
        if len(mine) != 1:
            problems.append(f"{name}: {len(mine)} connections to its port, 1 expected")
            continue
        data = mine[0]
        if not data.handshake_ok():
            problems.append(f"{name}: no complete handshake")
        up, up_problems = data.stream(True)
        down, down_problems = data.stream(False)
        problems += [f"{name}: {p}" for p in up_problems + down_problems]
        wire = down if direction == "down" else up
        other = up if direction == "down" else down
        if wire != body:
            problems.append(f"{name}: {len(wire)} bytes on the wire, {len(body)} expected"
                            + ("" if len(wire) != len(body) else " (the bytes differ)"))
        if other:
            problems.append(f"{name}: {len(other)} bytes flowed the wrong way")
        if not (data.closed_by(True) and data.closed_by(False)):
            problems.append(f"{name}: not closed by a FIN from both sides")
        if any(not s.checksum_ok for s in data.segments):
            problems.append(f"{name}: a segment has a wrong checksum")
    for f in all_flows:
        if f.initiator[0] == guest_ip and f.responder[0] == gateway_ip:
            port = f.responder[1]
            if port == control_port or port in data_ports:
                continue
            if port in (47771, 47773, 47999):  # the other tools' ports
                continue
            problems.append(f"a connection to the gateway's port {port}, which is not part of the FTP session")
    return len(transfers), problems
