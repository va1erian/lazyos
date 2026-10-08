#!/usr/bin/env python3
"""NTLMv2 and SMB 2.x signing for the harness server, standard library only.

An implementation independent of `libs/smbwire` (Python, not Rust), so the
harness catches a mistake the two sides would otherwise share. MD4 is written
out here because OpenSSL 3 builds of `hashlib` often lack it.

    NT_hash        = MD4(UTF-16LE(password))
    NTOWFv2        = HMAC-MD5(NT_hash, UTF-16LE(upper(user) + domain))
    NTProofStr     = HMAC-MD5(NTOWFv2, server_challenge + blob)
    SessionBaseKey = HMAC-MD5(NTOWFv2, NTProofStr)
"""

from __future__ import annotations

import hashlib
import hmac
import struct


def _rotl(x: int, n: int) -> int:
    x &= 0xFFFFFFFF
    return ((x << n) | (x >> (32 - n))) & 0xFFFFFFFF


def md4(data: bytes) -> bytes:
    """RFC 1320."""
    msg = data + b"\x80" + b"\x00" * ((55 - len(data)) % 64) + struct.pack("<Q", len(data) * 8)
    a, b, c, d = 0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476
    for at in range(0, len(msg), 64):
        x = struct.unpack("<16I", msg[at:at + 64])
        aa, bb, cc, dd = a, b, c, d

        def f(x_, y, z):
            return (x_ & y) | (~x_ & z)

        def g(x_, y, z):
            return (x_ & y) | (x_ & z) | (y & z)

        def h(x_, y, z):
            return x_ ^ y ^ z

        for i in range(16):
            k, s = i, (3, 7, 11, 19)[i % 4]
            if i % 4 == 0:
                a = _rotl(a + f(b, c, d) + x[k], s)
            elif i % 4 == 1:
                d = _rotl(d + f(a, b, c) + x[k], s)
            elif i % 4 == 2:
                c = _rotl(c + f(d, a, b) + x[k], s)
            else:
                b = _rotl(b + f(c, d, a) + x[k], s)
        for i in range(16):
            k, s = (i % 4) * 4 + i // 4, (3, 5, 9, 13)[i % 4]
            if i % 4 == 0:
                a = _rotl(a + g(b, c, d) + x[k] + 0x5A827999, s)
            elif i % 4 == 1:
                d = _rotl(d + g(a, b, c) + x[k] + 0x5A827999, s)
            elif i % 4 == 2:
                c = _rotl(c + g(d, a, b) + x[k] + 0x5A827999, s)
            else:
                b = _rotl(b + g(c, d, a) + x[k] + 0x5A827999, s)
        order = (0, 8, 4, 12, 2, 10, 6, 14, 1, 9, 5, 13, 3, 11, 7, 15)
        for i in range(16):
            k, s = order[i], (3, 9, 11, 15)[i % 4]
            if i % 4 == 0:
                a = _rotl(a + h(b, c, d) + x[k] + 0x6ED9EBA1, s)
            elif i % 4 == 1:
                d = _rotl(d + h(a, b, c) + x[k] + 0x6ED9EBA1, s)
            elif i % 4 == 2:
                c = _rotl(c + h(d, a, b) + x[k] + 0x6ED9EBA1, s)
            else:
                b = _rotl(b + h(c, d, a) + x[k] + 0x6ED9EBA1, s)
        a, b, c, d = ((a + aa) & 0xFFFFFFFF, (b + bb) & 0xFFFFFFFF,
                      (c + cc) & 0xFFFFFFFF, (d + dd) & 0xFFFFFFFF)
    return struct.pack("<4I", a, b, c, d)


def hmac_md5(key: bytes, *parts: bytes) -> bytes:
    return hmac.new(key, b"".join(parts), hashlib.md5).digest()


def nt_hash(password: str) -> bytes:
    return md4(password.encode("utf-16-le"))


def ntowfv2(password: str, user: str, domain: str) -> bytes:
    return hmac_md5(nt_hash(password), (user.upper() + domain).encode("utf-16-le"))


def check_ntlmv2(password: str, user: str, domain: str, server_challenge: bytes,
                 nt_response: bytes) -> bytes | None:
    """The session base key if `nt_response` proves the password, else None."""
    if len(nt_response) < 16 + 28:
        return None
    proof, blob = nt_response[:16], nt_response[16:]
    key = ntowfv2(password, user, domain)
    if not hmac.compare_digest(hmac_md5(key, server_challenge, blob), proof):
        return None
    return hmac_md5(key, proof)


def smb2_signature(key: bytes, message: bytes) -> bytes:
    """HMAC-SHA256 of the message with a zero signature field, first 16 bytes."""
    zeroed = message[:48] + b"\x00" * 16 + message[64:]
    return hmac.new(key, zeroed, hashlib.sha256).digest()[:16]


if __name__ == "__main__":
    # MS-NLMP 4.2.4 test vectors.
    assert md4(b"").hex() == "31d6cfe0d16ae931b73c59d7e0c089c0"
    assert md4(b"abc").hex() == "a448017aaf21d8525fc10ae87aa6729d"
    key = ntowfv2("Password", "User", "Domain")
    assert key.hex() == "0c868a403bfd7a93a3001ef22ef02e3f", key.hex()
    print("ntlm: vectors ok")
