"""CCMP-128 (IEEE 802.11-2020 12.5.3) on the access point side, over
`cryptography`'s AES-CCM: the AAD and nonce built here from the standard,
not shared with the guest's `ieee80211::ccmp`.
"""

from __future__ import annotations

from cryptography.hazmat.primitives.ciphers.aead import AESCCM
from cryptography.exceptions import InvalidTag

from .dot11 import ORDER, TO_DS, FROM_DS

MIC_LEN = 8
HEADER_LEN = 8


def ccmp_header(pn: int, key_id: int) -> bytes:
    p = pn.to_bytes(6, "little")
    return bytes([p[0], p[1], 0, 0x20 | ((key_id & 3) << 6), p[2], p[3], p[4], p[5]])


def parse_ccmp_header(data: bytes) -> tuple[int, int] | None:
    if len(data) < HEADER_LEN or not data[3] & 0x20:
        return None
    pn = int.from_bytes(bytes([data[0], data[1], data[4], data[5], data[6], data[7]]), "little")
    return pn, data[3] >> 6


def _aad_nonce(frame: bytes, header_len: int, pn: int) -> tuple[bytes, bytes]:
    fc0, flags = frame[0], frame[1]
    data = (fc0 >> 2) & 3 == 2
    qos = data and bool(fc0 & 0x80)
    if data:
        fc0 &= ~0x70 & 0xFF          # subtype bits b4-b6
    flags = (flags & ~(0x08 | 0x10 | 0x20) & 0xFF) | 0x40   # retry, pwr mgt, more data; protected
    if qos:
        flags &= ~ORDER & 0xFF
    aad = bytes([fc0, flags]) + frame[4:22] + bytes([frame[22] & 0x0F, 0])
    if (frame[1] & (TO_DS | FROM_DS)) == TO_DS | FROM_DS:
        aad += frame[24:30]
    tid = 0
    if qos:
        qc_at = 30 if (frame[1] & (TO_DS | FROM_DS)) == TO_DS | FROM_DS else 24
        tid = frame[qc_at] & 0x0F
        aad += bytes([tid, 0])
    nonce_flags = tid | (0x10 if not data else 0)
    nonce = bytes([nonce_flags]) + frame[10:16] + pn.to_bytes(6, "big")
    return aad, nonce


def seal(tk: bytes, frame: bytes, header_len: int) -> bytes:
    """Encrypt a frame whose CCMP header follows its MAC header; returns the
    frame with ciphertext and MIC."""
    pn, _ = parse_ccmp_header(frame[header_len:])
    aad, nonce = _aad_nonce(frame, header_len, pn)
    start = header_len + HEADER_LEN
    sealed = AESCCM(tk, tag_length=MIC_LEN).encrypt(nonce, frame[start:], aad)
    return frame[:start] + sealed


def open_frame(tk: bytes, frame: bytes, header_len: int) -> bytes | None:
    """The plaintext body (between the CCMP header and the MIC) of a
    protected frame, or None when the MIC does not verify."""
    parsed = parse_ccmp_header(frame[header_len:])
    if parsed is None or len(frame) < header_len + HEADER_LEN + MIC_LEN:
        return None
    aad, nonce = _aad_nonce(frame, header_len, parsed[0])
    try:
        return AESCCM(tk, tag_length=MIC_LEN).decrypt(nonce, frame[header_len + HEADER_LEN:], aad)
    except InvalidTag:
        return None


def self_test() -> None:
    """IEEE 802.11-2020 Annex J.6.4."""
    tk = bytes.fromhex("c97c1f67ce371185514a8a19f2bdd52f")
    header = bytes.fromhex("0848c32c0fd2e128a57c5030f1844408abaea5b8fcba8033")
    plain = bytes.fromhex("f8ba1a55d02f85ae967bb62fb6cda8eb7e78a050")
    frame = header + ccmp_header(0xB5039776E70C, 0) + plain
    sealed = seal(tk, frame, 24)
    expected = "f3d0a2fe9a3dbf2342a643e43246e80c3c04d0197845ce0b16f97623"
    assert sealed[32:].hex() == expected, sealed[32:].hex()
    assert sealed[24:32] == bytes.fromhex("0ce70020769703b5")
    assert open_frame(tk, sealed, 24) == plain
