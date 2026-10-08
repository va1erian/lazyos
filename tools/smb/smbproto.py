#!/usr/bin/env python3
"""SMB2 wire helpers for the harness server: the header, SPNEGO's DER, and the
NTLM CHALLENGE and AUTHENTICATE messages, written from `MS-SMB2`, `MS-NLMP` and
RFC 4178 independently of `libs/smbwire`.
"""

from __future__ import annotations

import struct
import time
from dataclasses import dataclass

PROTOCOL = b"\xfeSMB"
HEADER = 64

NEGOTIATE, SESSION_SETUP, LOGOFF, TREE_CONNECT, TREE_DISCONNECT = 0, 1, 2, 3, 4
CREATE, CLOSE, FLUSH, READ, WRITE = 5, 6, 7, 8, 9
ECHO, QUERY_DIRECTORY, QUERY_INFO, SET_INFO = 0x0D, 0x0E, 0x10, 0x11
NAMES = {NEGOTIATE: "NEGOTIATE", SESSION_SETUP: "SESSION_SETUP", LOGOFF: "LOGOFF",
         TREE_CONNECT: "TREE_CONNECT", TREE_DISCONNECT: "TREE_DISCONNECT", CREATE: "CREATE",
         CLOSE: "CLOSE", FLUSH: "FLUSH", READ: "READ", WRITE: "WRITE", ECHO: "ECHO",
         QUERY_DIRECTORY: "QUERY_DIRECTORY", QUERY_INFO: "QUERY_INFO", SET_INFO: "SET_INFO"}

FLAG_RESPONSE, FLAG_SIGNED = 0x1, 0x8

SUCCESS = 0
NO_MORE_FILES = 0x80000006
INVALID_PARAMETER = 0xC000000D
END_OF_FILE = 0xC0000011
MORE_PROCESSING_REQUIRED = 0xC0000016
ACCESS_DENIED = 0xC0000022
OBJECT_NAME_INVALID = 0xC0000033
OBJECT_NAME_NOT_FOUND = 0xC0000034
OBJECT_NAME_COLLISION = 0xC0000035
OBJECT_PATH_NOT_FOUND = 0xC000003A
LOGON_FAILURE = 0xC000006D
FILE_IS_A_DIRECTORY = 0xC00000BA
NOT_SUPPORTED = 0xC00000BB
NETWORK_NAME_DELETED = 0xC00000C9
BAD_NETWORK_NAME = 0xC00000CC
DIRECTORY_NOT_EMPTY = 0xC0000101
NOT_A_DIRECTORY = 0xC0000103
USER_SESSION_DELETED = 0xC0000203

SPNEGO_OID = bytes([0x2b, 0x06, 0x01, 0x05, 0x05, 0x02])
NTLMSSP_OID = bytes([0x2b, 0x06, 0x01, 0x04, 0x01, 0x82, 0x37, 0x02, 0x02, 0x0a])
NTLM_SIGNATURE = b"NTLMSSP\x00"


@dataclass
class Header:
    credit_charge: int
    status: int
    command: int
    credits: int
    flags: int
    next_command: int
    message_id: int
    tree_id: int
    session_id: int
    signature: bytes

    @classmethod
    def parse(cls, message: bytes) -> "Header":
        if len(message) < HEADER or message[:4] != PROTOCOL:
            raise ValueError("not an SMB2 message")
        (size, charge, status, command, credits, flags, nxt, mid, _pid, tid, sid) = struct.unpack_from(
            "<HHIHHIIQIIQ", message, 4)
        if size != HEADER:
            raise ValueError("header size")
        return cls(charge, status, command, credits, flags, nxt, mid, tid, sid, message[48:64])

    def response(self, status: int, session_id: int, tree_id: int, credits: int = 32) -> bytes:
        return (PROTOCOL + struct.pack("<HHIHHIIQIIQ", HEADER, self.credit_charge, status, self.command, credits,
                                       FLAG_RESPONSE, 0, self.message_id, 0xFEFF, tree_id, session_id)
                + b"\x00" * 16)


def frame(message: bytes) -> bytes:
    return struct.pack(">I", len(message)) + message


def filetime(seconds: float | None = None) -> int:
    return int(((time.time() if seconds is None else seconds) + 11644473600) * 10_000_000)


# ---- DER / SPNEGO ---------------------------------------------------------------

def der(tag: int, content: bytes) -> bytes:
    n = len(content)
    if n < 0x80:
        return bytes([tag, n]) + content
    raw = n.to_bytes((n.bit_length() + 7) // 8, "big")
    return bytes([tag, 0x80 | len(raw)]) + raw + content


def der_read(data: bytes) -> tuple[int, bytes, bytes]:
    """(tag, contents, rest) of the element at the start of `data`."""
    if len(data) < 2:
        raise ValueError("DER: short")
    tag, first = data[0], data[1]
    at = 2
    if first < 0x80:
        n = first
    else:
        count = first & 0x7F
        if not 0 < count <= 4 or len(data) < 2 + count:
            raise ValueError("DER: length")
        n = int.from_bytes(data[2:2 + count], "big")
        at += count
    if len(data) < at + n:
        raise ValueError("DER: overrun")
    return tag, data[at:at + n], data[at + n:]


def der_fields(data: bytes) -> dict[int, bytes]:
    out = {}
    while data:
        tag, contents, data = der_read(data)
        out[tag] = contents
    return out


def negotiate_hint() -> bytes:
    """What Samba puts in its NEGOTIATE response: a `NegTokenInit2` listing
    NTLMSSP, with the `not_defined_in_RFC4178@please_ignore` hint."""
    mechs = der(0xA0, der(0x30, der(0x06, NTLMSSP_OID)))
    hints = der(0xA3, der(0x30, der(0xA0, der(0x1B, b"not_defined_in_RFC4178@please_ignore"))))
    return der(0x60, der(0x06, SPNEGO_OID) + der(0xA0, der(0x30, mechs + hints)))


def unwrap_client_token(token: bytes) -> tuple[bytes, bool]:
    """The NTLM message in a client SESSION_SETUP token, and whether it was
    wrapped in SPNEGO."""
    if token.startswith(NTLM_SIGNATURE):
        return token, False
    tag, body, _ = der_read(token)
    if tag == 0x60:  # NegTokenInit inside the GSS-API header
        tag, oid, rest = der_read(body)
        if tag != 0x06 or oid != SPNEGO_OID:
            raise ValueError("not SPNEGO")
        tag, init, _ = der_read(rest)
        tag, seq, _ = der_read(init)
        fields = der_fields(seq)
        mechs = der_fields(der_read(fields[0xA0])[1]) if 0xA0 in fields else {}
        if not any(t == 0x06 for t in mechs) or NTLMSSP_OID not in fields[0xA0]:
            raise ValueError("NTLMSSP not offered")
        return der_read(fields[0xA2])[1], True
    if tag == 0xA1:  # NegTokenResp
        tag, seq, _ = der_read(body)
        fields = der_fields(seq)
        return der_read(fields[0xA2])[1], True
    raise ValueError("unknown token")


def neg_token_resp(state: int, token: bytes = b"") -> bytes:
    seq = der(0xA0, der(0x0A, bytes([state])))
    if state == 1:
        seq += der(0xA1, der(0x06, NTLMSSP_OID))
    if token:
        seq += der(0xA2, der(0x04, token))
    return der(0xA1, der(0x30, seq))


# ---- NTLM -------------------------------------------------------------------------

AV_EOL, AV_NB_COMPUTER, AV_NB_DOMAIN, AV_DNS_COMPUTER, AV_DNS_DOMAIN, AV_TIMESTAMP = 0, 1, 2, 3, 4, 7
NTLM_FLAGS = (0x00000001 | 0x00000004 | 0x00000010 | 0x00000200 | 0x00008000 | 0x00020000
              | 0x00080000 | 0x00800000 | 0x02000000 | 0x20000000 | 0x80000000)


def av_pairs(domain: str, computer: str, timestamp: int | None) -> bytes:
    def pair(av: int, value: bytes) -> bytes:
        return struct.pack("<HH", av, len(value)) + value

    out = pair(AV_NB_DOMAIN, domain.encode("utf-16-le")) + pair(AV_NB_COMPUTER, computer.encode("utf-16-le"))
    out += pair(AV_DNS_DOMAIN, domain.lower().encode("utf-16-le"))
    out += pair(AV_DNS_COMPUTER, computer.lower().encode("utf-16-le"))
    if timestamp is not None:
        out += pair(AV_TIMESTAMP, struct.pack("<Q", timestamp))
    return out + pair(AV_EOL, b"")


def challenge_message(server_challenge: bytes, domain: str, computer: str, timestamp: int | None) -> bytes:
    target = domain.encode("utf-16-le")
    info = av_pairs(domain, computer, timestamp)
    fixed = 56
    return (NTLM_SIGNATURE + struct.pack("<I", 2)
            + struct.pack("<HHI", len(target), len(target), fixed)
            + struct.pack("<I", NTLM_FLAGS) + server_challenge + b"\x00" * 8
            + struct.pack("<HHI", len(info), len(info), fixed + len(target))
            + bytes([6, 1, 0, 0, 0, 0, 0, 15]) + target + info)


@dataclass
class Authenticate:
    lm: bytes
    nt: bytes
    domain: str
    user: str
    workstation: str
    flags: int


def parse_authenticate(message: bytes) -> Authenticate:
    if not message.startswith(NTLM_SIGNATURE) or struct.unpack_from("<I", message, 8)[0] != 3:
        raise ValueError("not an AUTHENTICATE")

    def field(at: int) -> bytes:
        n, _, off = struct.unpack_from("<HHI", message, at)
        if off + n > len(message):
            raise ValueError("NTLM field overrun")
        return message[off:off + n]

    return Authenticate(field(12), field(20), field(28).decode("utf-16-le"), field(36).decode("utf-16-le"),
                        field(44).decode("utf-16-le"), struct.unpack_from("<I", message, 60)[0])
