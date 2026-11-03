"""Seeds for the `btsmp` fuzz target (`gen_corpus.py`).

The input is one SMP PDU (decode/encode round trip) and, read as a script, a
flags octet (bit 0 require an authenticated key, bit 1 offer our identity,
bit 2 allow a 7-octet key) then frames, each a big-endian u16 length and the
bytes, delivered to the LE Secure Connections central
(`libs/btsmp/src/fuzz.rs`). A frame starting 0xFF is an Encryption Change,
0xFE a user confirmation, 0xFD a late Tick. The public keys are real P-256
points (the Core Specification's sample keys, computed here so no secret is
typed in), so a script reaches the confirm exchange instead of stopping at the
curve check.
"""
import struct

P = 0xFFFFFFFF00000001000000000000000000000000FFFFFFFFFFFFFFFFFFFFFFFF
A = P - 3
GX = 0x6B17D1F2E12C4247F8BCE6E563A440F277037D812DEB33A0F4A13945D898C296
GY = 0x4FE342E2FE1A7F9B8EE7EB4A7C0F9E162BCE33576B315ECECBB6406837BF51F5

PRIV_A = 0x3F49F6D4A3C55F3874C9B3E3D2103F504AFF607BEB40B7995899B8A6CD3C1ABD
PRIV_B = 0x55188B3D32F6BB9A900AFCFBEED4E72A59CB9AC2F19D7CFB6B4FDD49F47FC5FD


def add(p, q):
    if p is None:
        return q
    if q is None:
        return p
    (x1, y1), (x2, y2) = p, q
    if x1 == x2 and (y1 + y2) % P == 0:
        return None
    if p == q:
        m = (3 * x1 * x1 + A) * pow(2 * y1, -1, P) % P
    else:
        m = (y2 - y1) * pow(x2 - x1, -1, P) % P
    x3 = (m * m - x1 - x2) % P
    return x3, (m * (x1 - x3) - y1) % P


def mul(k, point):
    out = None
    while k:
        if k & 1:
            out = add(out, point)
        point = add(point, point)
        k >>= 1
    return out


def public_le(priv):
    x, y = mul(priv, (GX, GY))
    return x.to_bytes(32, "little") + y.to_bytes(32, "little")


def frame(pdu):
    return struct.pack(">H", len(pdu)) + pdu


def script(flags, *pdus):
    return bytes([flags]) + b"".join(frame(p) for p in pdus)


def features(code, io, auth=0x0D, oob=0, key=16, init=0, resp=2):
    return bytes([code, io, oob, auth, key, init, resp])


def response(io, **kw):
    return features(2, io, **kw)


def seeds():
    pub_b = public_le(PRIV_B)
    pub_debug = public_le(PRIV_A)
    off_curve = pub_b[:32] + bytes([pub_b[32] ^ 1]) + pub_b[33:]
    confirm = bytes([3]) + bytes(range(16))
    random = bytes([4]) + bytes(range(16, 32))
    dhkey = bytes([0x0D]) + bytes(range(32, 48))
    ident = bytes([8]) + b"\x5a" * 16
    ident_addr = bytes([9, 1]) + bytes([1, 2, 3, 4, 5, 0xC6])
    pk = bytes([0x0C]) + pub_b

    def flow(io, *tail, flags=2):
        return script(flags, response(io), pk, confirm, random, *tail)

    return {
        "pdu_pairing_request": features(1, 1, init=2),
        "pdu_pairing_response": response(3),
        "pdu_public_key": pk,
        "pdu_failed": bytes([5, 4]),
        "pdu_central_identification": bytes([7, 0x34, 0x12]) + bytes(range(8)),
        "pdu_security_request": bytes([0x0B, 0x0D]),
        "just_works_to_the_check": flow(3, dhkey),
        "just_works_through_keys": flow(3, dhkey, b"\xff", ident, ident_addr),
        "passkey_entry": flow(2, confirm, random, confirm, random, dhkey, flags=3),
        "numeric_comparison": flow(1, b"\xfe\x00", dhkey),
        "numeric_comparison_rejected": flow(4, b"\xfe\x01"),
        "authenticated_required_just_works": script(1, response(3)),
        "legacy_pairing": script(0, response(3, auth=0x05)),
        "short_key_refused": script(0, response(3, key=7)),
        "short_key_allowed": script(4, response(3, key=7), pk),
        "oob_flag": script(0, response(3, oob=1)),
        "keys_not_asked_for": script(0, response(3, init=0x0A, resp=0x0F)),
        "invalid_curve_point": script(0, response(3), bytes([0x0C]) + off_curve),
        "debug_key": script(0, response(3), bytes([0x0C]) + pub_debug),
        "peer_failed": script(0, response(3), bytes([5, 8])),
        "keypress_flood": script(0, response(2), *([bytes([0x0E, 1])] * 8)),
        "security_request_midway": script(0, response(3), bytes([0x0B, 0x0D]), pk),
        "keys_before_encryption": flow(3, dhkey, ident, ident_addr),
        "encryption_failed": flow(3, dhkey, b"\xff\x01"),
        "unknown_command": script(0, bytes([0x7F, 0, 0]), response(3)),
        "timeout": script(0, response(3), b"\xfd"),
        "empty": b"",
    }
