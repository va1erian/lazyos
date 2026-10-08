#!/usr/bin/env python3
"""The wire verdict on the guest's SMB sessions (docs/smb-plan.md §9, "Wire").

Every guest connection to a harness SMB port is reassembled
(`tools/net/sockets_pcap.py`) and split into SMB2 messages, and judged:

* no secret (the password, in ASCII or UTF-16LE) anywhere on the flow;
* the client offers dialect 2.1, and the server's choice is the expected one;
* the TREE_CONNECT names the expected share;
* signing: on a signing port every message after the logon is signed, both
  ways; on a plain port no request is;
* file bytes: each expected upload is rebuilt from WRITE requests alone and
  each expected download from READ responses alone, and an upload's bytes do
  not appear anywhere else in the client's stream.
"""

from __future__ import annotations

import struct
import sys
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "net"))
import sockets_pcap  # noqa: E402

NEGOTIATE, SESSION_SETUP, TREE_CONNECT, READ, WRITE = 0, 1, 3, 8, 9
FLAG_RESPONSE, FLAG_ASYNC, FLAG_SIGNED = 0x1, 0x2, 0x8
PENDING = 0x00000103


@dataclass
class Message:
    command: int
    status: int
    flags: int
    message_id: int
    raw: bytes

    @property
    def body(self) -> bytes:
        return self.raw[64:]


@dataclass
class PortExpect:
    """What one harness port's flows must show."""
    dialect: int | None = 0x0210
    share: str | None = "share"
    signed: bool | None = None
    uploads: list[bytes] = field(default_factory=list)
    downloads: list[bytes] = field(default_factory=list)
    min_flows: int = 1


def split(stream: bytes) -> tuple[list[Message], list[str]]:
    """The SMB2 messages of one direction of a flow."""
    out, at = [], 0
    while at + 4 <= len(stream):
        if stream[at] != 0:
            return out, [f"a frame at byte {at} is not a session message"]
        size = struct.unpack_from(">I", stream, at)[0] & 0xFFFFFF
        raw = stream[at + 4:at + 4 + size]
        if len(raw) < size:
            return out, [f"a frame at byte {at} is cut short"]
        if len(raw) < 64 or raw[:4] != b"\xfeSMB":
            return out, [f"a frame at byte {at} is not SMB2"]
        status, command, _, flags, _, mid = struct.unpack_from("<IHHIIQ", raw, 8)
        out.append(Message(command, status, flags, mid, raw))
        at += 4 + size
    return out, ([f"{len(stream) - at} stray bytes at the end"] if at != len(stream) else [])


def _rebuild(pieces: list[tuple[int, int, bytes]]) -> dict[int, bytes]:
    """Per file id, the bytes the pieces (file id, offset, data) cover."""
    files: dict[int, bytearray] = {}
    for fid, offset, data in pieces:
        buf = files.setdefault(fid, bytearray())
        if len(buf) < offset + len(data):
            buf.extend(b"\x00" * (offset + len(data) - len(buf)))
        buf[offset:offset + len(data)] = data
    return {fid: bytes(buf) for fid, buf in files.items()}


def writes(requests: list[Message]) -> tuple[list[tuple[int, int, bytes]], bytes]:
    """The data every WRITE carried, and the client stream without it."""
    pieces, outside = [], bytearray()
    for m in requests:
        if m.command == WRITE:
            data_off, length, offset = struct.unpack_from("<HIQ", m.body, 2)
            fid = struct.unpack_from("<Q", m.body, 16)[0]
            pieces.append((fid, offset, m.raw[data_off:data_off + length]))
            outside += m.raw[:data_off] + m.raw[data_off + length:]
        else:
            outside += m.raw
    return pieces, bytes(outside)


def reads(requests: list[Message], responses: list[Message]) -> list[tuple[int, int, bytes]]:
    asked = {}
    for m in requests:
        if m.command == READ:
            offset = struct.unpack_from("<Q", m.body, 8)[0]
            fid = struct.unpack_from("<Q", m.body, 16)[0]
            asked[m.message_id] = (fid, offset)
    pieces = []
    for m in responses:
        if m.command == READ and m.status == 0 and m.message_id in asked:
            data_off, length = m.body[2], struct.unpack_from("<I", m.body, 4)[0]
            pieces.append((*asked[m.message_id], m.raw[data_off:data_off + length]))
    return pieces


def _signing_problems(requests: list[Message], responses: list[Message], want: bool, where: str) -> list[str]:
    problems = []
    logged_on = next((m.message_id for m in responses if m.command == SESSION_SETUP and m.status == 0), None)
    if logged_on is None:
        return []  # a refused logon has nothing to sign
    after_req = [m for m in requests if m.message_id > logged_on]
    after_resp = [m for m in responses if m.message_id > logged_on
                  and not (m.flags & FLAG_ASYNC and m.status == PENDING)]
    if want:
        unsigned = [m.command for m in after_req + after_resp if not m.flags & FLAG_SIGNED]
        if unsigned:
            problems.append(f"{where}: {len(unsigned)} unsigned message(s) after the logon (commands {unsigned[:5]})")
        if not after_req:
            problems.append(f"{where}: no requests after the logon")
    elif any(m.flags & FLAG_SIGNED for m in after_req):
        problems.append(f"{where}: requests are signed though signing was neither required nor asked for")
    return problems


def check_flow(client: bytes, server: bytes, expect: PortExpect, where: str) -> tuple[list[str], dict]:
    requests, problems = split(client)
    responses, more = split(server)
    problems += more
    seen = {"uploads": set(), "downloads": set(), "share": False,
            "logged_on": any(m.command == SESSION_SETUP and m.status == 0 for m in responses)}
    negotiate = next((m for m in requests if m.command == NEGOTIATE), None)
    if negotiate is None:
        return problems + [f"{where}: no NEGOTIATE"], seen
    count = struct.unpack_from("<H", negotiate.body, 2)[0]
    offered = struct.unpack_from(f"<{count}H", negotiate.body, 36)
    if 0x0210 not in offered:
        problems.append(f"{where}: the client offered {[hex(d) for d in offered]}, not 0x0210")
    chosen = next((m for m in responses if m.command == NEGOTIATE and m.status == 0), None)
    if expect.dialect is not None:
        dialect = struct.unpack_from("<H", chosen.body, 4)[0] if chosen else None
        if dialect != expect.dialect:
            problems.append(f"{where}: dialect {dialect}, expected 0x{expect.dialect:04x}")
    if expect.share is not None:
        trees = [m for m in requests if m.command == TREE_CONNECT]
        paths = []
        for m in trees:
            off, length = struct.unpack_from("<HH", m.body, 4)
            paths.append(m.raw[off:off + length].decode("utf-16-le", "replace"))
        seen["share"] = any(p.lower().endswith("\\" + expect.share.lower()) for p in paths)
    if expect.signed is not None:
        problems += _signing_problems(requests, responses, expect.signed, where)
    pieces, outside = writes(requests)
    uploaded = _rebuild(pieces)
    downloaded = _rebuild(reads(requests, responses))
    for index, body in enumerate(expect.uploads):
        if body in uploaded.values():
            seen["uploads"].add(index)
        for probe in (body[:64], body[len(body) // 2:len(body) // 2 + 64]):
            if len(probe) >= 32 and probe in outside:
                problems.append(f"{where}: upload {index}'s bytes appear outside WRITE data")
    for index, body in enumerate(expect.downloads):
        if body in downloaded.values():
            seen["downloads"].add(index)
    return problems, seen


def check_smb_flows(frames, guest_ip: bytes, gateway_ip: bytes, expected: dict[int, PortExpect],
                    secrets: list[bytes]) -> tuple[int, list[str]]:
    """Judge every guest flow to a port in `expected`; (flows seen, problems)."""
    problems: list[str] = []
    counts: dict[int, int] = {}
    seen: dict[int, dict] = {port: {"uploads": set(), "downloads": set(), "share": False, "logged_on": False}
                             for port in expected}
    for flow in sockets_pcap.flows(frames):
        ip, port = flow.responder
        if flow.initiator[0] != guest_ip or ip != gateway_ip or port not in expected:
            continue
        counts[port] = counts.get(port, 0) + 1
        where = f"port {port} flow {counts[port]}"
        client, gaps = flow.stream(True)
        server, more = flow.stream(False)
        problems += [f"{where}: {g}" for g in gaps + more]
        for secret in secrets:
            if secret and (secret in client or secret in server):
                problems.append(f"{where} carries a secret in the clear")
        found, flow_seen = check_flow(client, server, expected[port], where)
        problems += found
        for key in ("uploads", "downloads", "share", "logged_on"):
            seen[port][key] |= flow_seen[key]
    for port, expect in expected.items():
        if counts.get(port, 0) < expect.min_flows:
            problems.append(f"port {port}: {counts.get(port, 0)} connections, expected at least {expect.min_flows}")
        if expect.share is not None and not seen[port]["share"]:
            problems.append(f"port {port}: no TREE_CONNECT for {expect.share!r}")
        if expect.signed and not seen[port]["logged_on"]:
            problems.append(f"port {port}: no session logged on to sign")
        for key, bodies in (("uploads", expect.uploads), ("downloads", expect.downloads)):
            missing = set(range(len(bodies))) - seen[port][key]
            if missing:
                problems.append(f"port {port}: {key} {sorted(missing)} never crossed the wire whole")
    return sum(counts.values()), problems
