"""The WPA2-PSK authenticator's cryptography and EAPOL-Key frames (IEEE
802.11-2020 12.7), from the standard with `hashlib`, `hmac` and
`cryptography` alone: an independent other side for `libs/eapol`, so a
mistake both could share cannot pass.
"""

from __future__ import annotations

import hashlib
import hmac
import os
import struct
from dataclasses import dataclass

from cryptography.hazmat.primitives.keywrap import aes_key_wrap

EAPOL_VERSION = 2
EAPOL_KEY = 3
DESCRIPTOR_RSN = 2
# Key Information bits.
VERSION_AES = 2
PAIRWISE, INSTALL, ACK, MIC, SECURE, ENCRYPTED = 0x0008, 0x0040, 0x0080, 0x0100, 0x0200, 0x1000
MSG1 = VERSION_AES | PAIRWISE | ACK
MSG3 = VERSION_AES | PAIRWISE | INSTALL | ACK | MIC | SECURE | ENCRYPTED
GROUP1 = VERSION_AES | ACK | MIC | SECURE | ENCRYPTED
#: Offsets in the EAPOL frame (from the EAPOL version octet).
KEY_INFO, NONCE, RSC, MIC_AT, DATA_LEN = 5, 17, 65, 81, 97
FIXED = DATA_LEN + 2


def pmk(passphrase: str, ssid: bytes) -> bytes:
    return hashlib.pbkdf2_hmac("sha1", passphrase.encode(), ssid, 4096, 32)


def prf(key: bytes, label: bytes, data: bytes, bits: int) -> bytes:
    out = b""
    counter = 0
    while len(out) * 8 < bits:
        out += hmac.new(key, label + b"\0" + data + bytes([counter]), hashlib.sha1).digest()
        counter += 1
    return out[:bits // 8]


@dataclass
class Ptk:
    kck: bytes
    kek: bytes
    tk: bytes


def ptk(pmk_: bytes, aa: bytes, spa: bytes, anonce: bytes, snonce: bytes) -> Ptk:
    data = min(aa, spa) + max(aa, spa) + min(anonce, snonce) + max(anonce, snonce)
    raw = prf(pmk_, b"Pairwise key expansion", data, 384)
    return Ptk(raw[:16], raw[16:32], raw[32:48])


def mic(kck: bytes, frame: bytes) -> bytes:
    zeroed = frame[:MIC_AT] + bytes(16) + frame[MIC_AT + 16:]
    return hmac.new(kck, zeroed, hashlib.sha1).digest()[:16]


@dataclass
class KeyFrame:
    info: int
    key_length: int
    replay: int
    nonce: bytes
    rsc: bytes
    mic: bytes
    data: bytes
    raw: bytes


def parse(frame: bytes) -> KeyFrame | None:
    """An EAPOL-Key frame (from the EAPOL version octet), or None."""
    if len(frame) < FIXED or frame[1] != EAPOL_KEY or frame[4] != DESCRIPTOR_RSN:
        return None
    body_len = struct.unpack(">H", frame[2:4])[0]
    data_len = struct.unpack(">H", frame[DATA_LEN:FIXED])[0]
    if 4 + body_len > len(frame) or FIXED + data_len > 4 + body_len:
        return None
    info, key_length, replay = struct.unpack(">HHQ", frame[KEY_INFO:NONCE])
    return KeyFrame(info, key_length, replay, frame[NONCE:NONCE + 32], frame[RSC:RSC + 8],
                    frame[MIC_AT:MIC_AT + 16], frame[FIXED:FIXED + data_len], frame[:4 + body_len])


def build(info: int, key_length: int, replay: int, nonce: bytes = bytes(32), rsc: bytes = bytes(8),
          data: bytes = b"", kck: bytes | None = None) -> bytes:
    body = (bytes([DESCRIPTOR_RSN]) + struct.pack(">HHQ", info, key_length, replay) + nonce
            + bytes(16) + rsc + bytes(8) + bytes(16) + struct.pack(">H", len(data)) + data)
    frame = bytes([EAPOL_VERSION, EAPOL_KEY]) + struct.pack(">H", len(body)) + body
    if kck is not None:
        frame = frame[:MIC_AT] + mic(kck, frame) + frame[MIC_AT + 16:]
    return frame


def verify(kck: bytes, frame: KeyFrame) -> bool:
    return hmac.compare_digest(mic(kck, frame.raw), frame.mic)


def gtk_kde(index: int, gtk: bytes) -> bytes:
    """A GTK KDE: dd, length, 00-0F-AC, type 1, key id (Tx set), reserved, GTK."""
    body = bytes([0x00, 0x0F, 0xAC, 1, (index & 3) | 0x04, 0]) + gtk
    return bytes([0xDD, len(body)]) + body


def wrap(kek: bytes, plain: bytes) -> bytes:
    """Pad key data (dd 00 ... to a multiple of 8, at least 16) and wrap it."""
    if len(plain) < 16 or len(plain) % 8:
        plain += b"\xdd"
        while len(plain) < 16 or len(plain) % 8:
            plain += b"\0"
    return aes_key_wrap(kek, plain)


def nonce() -> bytes:
    return os.urandom(32)


def self_test() -> None:
    """IEEE 802.11-2020 Annex J.4: the PSK of "password"/"IEEE"."""
    expected = "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e"
    assert pmk("password", b"IEEE").hex() == expected
    frame = build(MSG1, 16, 1, nonce=bytes(range(32)))
    parsed = parse(frame)
    assert parsed is not None and parsed.info == MSG1 and parsed.nonce == bytes(range(32))
    signed = build(MSG3, 16, 2, data=b"x" * 24, kck=bytes(16))
    assert verify(bytes(16), parse(signed))
