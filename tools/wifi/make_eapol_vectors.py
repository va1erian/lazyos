#!/usr/bin/env python3
"""Independent WPA2-PSK handshake transcripts for `libs/eapol`.

Writes `libs/eapol/src/tests/vectors.rs`: for AKM 2 (PSK, HMAC-SHA1-128 MIC,
PRF-SHA1) and AKM 6 (PSK-SHA256, AES-128-CMAC MIC, KDF-SHA256) the PMK, PTK,
the four pairwise messages and the two group messages, with fixed nonces,
replay counters and keys. Nothing here imports the Rust code or its crypto:
`hashlib`, `hmac` and `cryptography` (AES key wrap, AES-CMAC) only, and the
frame layout is written out from IEEE 802.11-2020 12.7.2 / 12.7.6 on its own.
The Rust tests feed the authenticator's messages (1, 3, group 1) to the
supplicant with the SNonce pinned and require its replies (2, 4, group 2) to
equal these bytes, so a symmetric mistake in `libs/eapol` cannot pass.

The script checks its own primitives first against IEEE 802.11-2020 Annex J
(J.3 PRF cases 1 and 2, J.4 passphrase "password"/"IEEE").

    python tools/wifi/make_eapol_vectors.py            # rewrite the fixture
    python tools/wifi/make_eapol_vectors.py --check    # fail if it is stale
"""
import argparse
import hashlib
import hmac
import struct
import sys
from pathlib import Path

from cryptography.hazmat.primitives import cmac
from cryptography.hazmat.primitives.ciphers import algorithms
from cryptography.hazmat.primitives.keywrap import aes_key_wrap

OUT = Path(__file__).resolve().parents[2] / "libs" / "eapol" / "src" / "tests" / "vectors.rs"

PASSPHRASE = b"LazyOS-Test-Pass"
SSID = b"LazyNet"
AA = bytes.fromhex("021122334455")
SPA = bytes.fromhex("02aabbccddee")
ANONCE = bytes(0x10 + i for i in range(32))
SNONCE = bytes(0x80 + i for i in range(32))
GNONCE = bytes(0xE0 + i for i in range(32))
GTK = bytes(0xC0 + i for i in range(16))
GTK_INDEX = 1
MSG3_RSC = bytes([5, 0, 0, 0, 0, 0, 0, 0])
NEW_GTK = bytes(0xD0 + i for i in range(16))
NEW_GTK_INDEX = 2
GROUP_RSC = bytes([9, 0, 0, 0, 0, 0, 0, 0])
# Replay counters of messages 1, 3 and group 1 (2 and 4 and group 2 echo them).
COUNTERS = (1, 2, 3)

# Key information bits (12.7.2).
PAIRWISE, INSTALL, ACK, MIC, SECURE, ENCRYPTED = 1 << 3, 1 << 6, 1 << 7, 1 << 8, 1 << 9, 1 << 12


def prf(key, label, data, nbytes):
    """802.11 PRF-n with HMAC-SHA1 (12.7.1.2)."""
    out = b""
    counter = 0
    while len(out) < nbytes:
        out += hmac.new(key, label + b"\0" + data + bytes([counter]), hashlib.sha1).digest()
        counter += 1
    return out[:nbytes]


def kdf(key, label, context, nbytes):
    """802.11 KDF with HMAC-SHA256 (12.7.1.7.2)."""
    out = b""
    i = 1
    while len(out) < nbytes:
        out += hmac.new(key, struct.pack("<H", i) + label + context + struct.pack("<H", nbytes * 8),
                        hashlib.sha256).digest()
        i += 1
    return out[:nbytes]


def self_check():
    """Annex J: J.3 PRF cases 1 and 2, J.4 PBKDF2 passphrase vector."""
    assert prf(b"\x0b" * 20, b"prefix", b"Hi There", 24).hex() == \
        "bcd4c650b30b9684951829e0d75f9d54b862175ed9f00606"
    assert prf(b"Jefe", b"prefix-2", b"what do ya want for nothing?", 24).hex() == \
        "47c4908e30c947521ad20be9053450ecbea23d3aa604b773"
    assert hashlib.pbkdf2_hmac("sha1", b"password", b"IEEE", 4096, 32).hex() == \
        "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e"


def rsn_ie(akm_type):
    """WPA2-Personal: CCMP group, one CCMP pairwise, one AKM, no capabilities."""
    suite = b"\x00\x0f\xac"
    body = (struct.pack("<H", 1) + suite + b"\x04" + struct.pack("<H", 1) + suite + b"\x04"
            + struct.pack("<H", 1) + suite + bytes([akm_type]) + struct.pack("<H", 0))
    return bytes([48, len(body)]) + body


def eapol_key(version, flags, key_len, replay, nonce, rsc, key_data, mic=b"\0" * 16):
    body = (bytes([2]) + struct.pack(">H", version | flags) + struct.pack(">H", key_len)
            + struct.pack(">Q", replay) + nonce + b"\0" * 16 + rsc + b"\0" * 8 + mic
            + struct.pack(">H", len(key_data)) + key_data)
    return bytes([2, 3]) + struct.pack(">H", len(body)) + body


def signed(frame, mic_fn, kck):
    """Compute the MIC over the frame with the MIC field zero and insert it."""
    mic = mic_fn(kck, frame)
    return frame[:81] + mic + frame[97:]


def gtk_kde(index, gtk):
    return b"\xdd" + bytes([4 + 2 + len(gtk)]) + b"\x00\x0f\xac\x01" + bytes([index & 3, 0]) + gtk


def pad(data):
    """Key data padding: a dd octet and zeros up to a multiple of 8 (>= 16)."""
    if len(data) % 8 == 0 and len(data) >= 16:
        return data
    data += b"\xdd"
    while len(data) % 8 or len(data) < 16:
        data += b"\0"
    return data


def vector(akm):
    sha256 = akm == 6
    version = 3 if sha256 else 2
    pmk = hashlib.pbkdf2_hmac("sha1", PASSPHRASE, SSID, 4096, 32)
    data = min(AA, SPA) + max(AA, SPA) + min(ANONCE, SNONCE) + max(ANONCE, SNONCE)
    label = b"Pairwise key expansion"
    ptk = (kdf if sha256 else prf)(pmk, label, data, 48)
    kck, kek, tk = ptk[:16], ptk[16:32], ptk[32:]
    if sha256:
        def mic_fn(key, frame):
            c = cmac.CMAC(algorithms.AES(key))
            c.update(frame)
            return c.finalize()
    else:
        def mic_fn(key, frame):
            return hmac.new(key, frame, hashlib.sha1).digest()[:16]
    ie = rsn_ie(akm)
    c1, c3, g1 = COUNTERS
    msg1 = eapol_key(version, PAIRWISE | ACK, 16, c1, ANONCE, bytes(8), b"")
    msg2 = signed(eapol_key(version, PAIRWISE | MIC, 0, c1, SNONCE, bytes(8), ie), mic_fn, kck)
    wrapped3 = aes_key_wrap(kek, pad(ie + gtk_kde(GTK_INDEX, GTK)))
    msg3 = signed(eapol_key(version, PAIRWISE | INSTALL | ACK | MIC | SECURE | ENCRYPTED, 16, c3,
                            ANONCE, MSG3_RSC, wrapped3), mic_fn, kck)
    msg4 = signed(eapol_key(version, PAIRWISE | MIC | SECURE, 0, c3, bytes(32), bytes(8), b""),
                  mic_fn, kck)
    wrapped_g = aes_key_wrap(kek, pad(gtk_kde(NEW_GTK_INDEX, NEW_GTK)))
    group1 = signed(eapol_key(version, ACK | MIC | SECURE | ENCRYPTED, 16, g1, GNONCE, GROUP_RSC,
                              wrapped_g), mic_fn, kck)
    group2 = signed(eapol_key(version, MIC | SECURE, 0, g1, bytes(32), bytes(8), b""), mic_fn, kck)
    return {
        "akm": akm, "pmk": pmk, "ptk": ptk, "tk": tk, "ap_rsn_ie": ie,
        "msg1": msg1, "msg2": msg2, "msg3": msg3, "msg4": msg4, "group1": group1, "group2": group2,
    }


def render():
    self_check()
    lines = [
        "// @generated by tools/wifi/make_eapol_vectors.py: do not edit.",
        "//! Independent handshake transcripts (see the generator for the inputs).",
        "",
        "pub struct Transcript {",
        "    pub akm: u8,",
        "    pub pmk: &'static str,",
        "    pub ptk: &'static str,",
        "    pub tk: &'static str,",
        "    pub ap_rsn_ie: &'static str,",
        "    pub msg1: &'static str,",
        "    pub msg2: &'static str,",
        "    pub msg3: &'static str,",
        "    pub msg4: &'static str,",
        "    pub group1: &'static str,",
        "    pub group2: &'static str,",
        "}",
        "",
        f'pub const PASSPHRASE: &str = "{PASSPHRASE.decode()}";',
        f'pub const SSID: &str = "{SSID.decode()}";',
        f'pub const AA: &str = "{AA.hex()}";',
        f'pub const SPA: &str = "{SPA.hex()}";',
        f'pub const ANONCE: &str = "{ANONCE.hex()}";',
        f'pub const SNONCE: &str = "{SNONCE.hex()}";',
        f'pub const GTK: &str = "{GTK.hex()}";',
        f"pub const GTK_INDEX: u8 = {GTK_INDEX};",
        f'pub const MSG3_RSC: &str = "{MSG3_RSC.hex()}";',
        f'pub const NEW_GTK: &str = "{NEW_GTK.hex()}";',
        f"pub const NEW_GTK_INDEX: u8 = {NEW_GTK_INDEX};",
        f'pub const GROUP_RSC: &str = "{GROUP_RSC.hex()}";',
        f"pub const COUNTERS: [u64; 3] = {list(COUNTERS)};",
        "",
        "pub const TRANSCRIPTS: [Transcript; 2] = [",
    ]
    for akm in (2, 6):
        v = vector(akm)
        lines.append("    Transcript {")
        lines.append(f"        akm: {akm},")
        for name in ("pmk", "ptk", "tk", "ap_rsn_ie", "msg1", "msg2", "msg3", "msg4", "group1", "group2"):
            lines.append(f'        {name}: "{v[name].hex()}",')
        lines.append("    },")
    lines.append("];")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="fail if the checked-in fixture differs")
    args = parser.parse_args()
    text = render().encode()
    if args.check:
        # Compare ignoring CRLF, which Python writes on Windows.
        have = OUT.read_bytes().replace(b"\r\n", b"\n") if OUT.exists() else None
        if have != text:
            print(f"stale: {OUT} (run tools/wifi/make_eapol_vectors.py)")
            return 1
        print("eapol vectors are current")
        return 0
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_bytes(text)
    print(f"wrote {OUT}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
