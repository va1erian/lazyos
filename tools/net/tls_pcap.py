"""The wire's side of the TLS verdict (docs/tls-plan.md §8).

From the capture: every TCP connection the guest opened to a TLS port starts
with a ClientHello naming the expected server (SNI) and offering ALPN
`http/1.1`; nothing on those connections is plaintext HTTP in either
direction; and no byte string the harness knows must stay secret (the pages
the servers sent, which are only ever sent encrypted) appears anywhere in the
capture's TLS flows.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass

import sockets_pcap

HANDSHAKE, CLIENT_HELLO = 0x16, 0x01
EXT_SERVER_NAME, EXT_ALPN = 0, 16


@dataclass
class ClientHello:
    sni: str | None
    alpn: list[str]
    suites: list[int]


def _handshake_bytes(stream: bytes) -> bytes:
    """The handshake messages carried by the leading run of handshake records."""
    out = bytearray()
    at = 0
    while at + 5 <= len(stream) and stream[at] == HANDSHAKE:
        length = struct.unpack(">H", stream[at + 3:at + 5])[0]
        out += stream[at + 5:at + 5 + length]
        at += 5 + length
    return bytes(out)


def _vector(data: bytes, at: int, size: int) -> tuple[bytes, int]:
    if at + size > len(data):
        raise ValueError("truncated length")
    length = int.from_bytes(data[at:at + size], "big")
    if at + size + length > len(data):
        raise ValueError("truncated vector")
    return data[at + size:at + size + length], at + size + length


def _extensions(block: bytes) -> dict[int, bytes]:
    found: dict[int, bytes] = {}
    at = 0
    while at + 4 <= len(block):
        kind = struct.unpack(">H", block[at:at + 2])[0]
        body, at = _vector(block, at + 2, 2)
        found[kind] = body
    return found


def _sni(body: bytes) -> str | None:
    names, _ = _vector(body, 0, 2)
    at = 0
    while at + 3 <= len(names):
        kind = names[at]
        name, at = _vector(names, at + 1, 2)
        if kind == 0:
            return name.decode("ascii", "replace")
    return None


def _alpn(body: bytes) -> list[str]:
    protocols, _ = _vector(body, 0, 2)
    out, at = [], 0
    while at < len(protocols):
        name, at = _vector(protocols, at, 1)
        out.append(name.decode("ascii", "replace"))
    return out


def parse_client_hello(stream: bytes) -> ClientHello | None:
    """The ClientHello at the start of a client's stream, or None."""
    data = _handshake_bytes(stream)
    if len(data) < 4 or data[0] != CLIENT_HELLO:
        return None
    try:
        body, _ = _vector(data, 1, 3)
        at = 2 + 32  # legacy_version, random
        _, at = _vector(body, at, 1)  # session id
        suites_raw, at = _vector(body, at, 2)
        _, at = _vector(body, at, 1)  # compression methods
        extensions, _ = _vector(body, at, 2) if at < len(body) else (b"", at)
        ext = _extensions(extensions)
        suites = [struct.unpack(">H", suites_raw[i:i + 2])[0] for i in range(0, len(suites_raw) - 1, 2)]
        return ClientHello(_sni(ext[EXT_SERVER_NAME]) if EXT_SERVER_NAME in ext else None,
                           _alpn(ext[EXT_ALPN]) if EXT_ALPN in ext else [], suites)
    except ValueError:
        return None


def check_tls_flows(frames, guest_ip: bytes, gateway_ip: bytes, expected: dict[int, str],
                    min_flows: dict[int, int], secrets: list[bytes]) -> tuple[int, list[str]]:
    """Judge every guest-initiated flow to a port in `expected` (port -> SNI).

    `min_flows` is the fewest connections each port must have seen; `secrets`
    are byte strings that must never appear in the clear on those flows.
    """
    problems: list[str] = []
    seen: dict[int, int] = {}
    for flow in sockets_pcap.flows(frames):
        ip, port = flow.responder
        if flow.initiator[0] != guest_ip or ip != gateway_ip or port not in expected:
            continue
        seen[port] = seen.get(port, 0) + 1
        client, _ = flow.stream(True)
        server, _ = flow.stream(False)
        where = f"flow {seen[port]} to port {port}"
        hello = parse_client_hello(client)
        if hello is None:
            problems.append(f"{where} does not start with a ClientHello")
            continue
        if hello.sni != expected[port]:
            problems.append(f"{where}: SNI {hello.sni!r}, expected {expected[port]!r}")
        if "http/1.1" not in hello.alpn:
            problems.append(f"{where}: ALPN {hello.alpn} does not offer http/1.1")
        if b"HTTP/1.1" in client or b"HTTP/1." in server:
            problems.append(f"{where} carries plaintext HTTP")
        for secret in secrets:
            if secret and (secret in client or secret in server):
                problems.append(f"{where} carries a secret in the clear ({secret[:16]!r}...)")
    for port, count in min_flows.items():
        if seen.get(port, 0) < count:
            problems.append(f"port {port}: {seen.get(port, 0)} TLS connections, expected at least {count}")
    return sum(seen.values()), problems
