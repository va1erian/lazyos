"""Seeds for the `btfw` fuzz target (`gen_corpus.py`).

`btfw` (libs/btfw/src/fuzz.rs) uses its input two ways: as one blob handed to
every container parser (Intel `.sfi` in all three layouts, the version TLV
list, the legacy version record, the boot parameters, a `.ddc` file, a Realtek
epatch file), and as a script for a firmware loader: byte 0 picks the vendor
(even Intel, odd Realtek), then records `tag:u8, length:u16 big endian,
bytes`, tag 0 an event packet, 1 a file, 2 "no file", 3 a tick (first byte
times 100 ms), 4 a detach.

The images here are built by an independent encoder, and the two "load"
scripts hold the exact event sequence a real controller sends, so the loader
under test is driven to the end: libs/btfw/src/tests/seeds.rs checks that they
finish with the expected outcome. Fragment counts are computed here from the
documented rules (252 octets per Secure_Send, command groups aligned to 4
octets), not read from the Rust code.
"""
import struct

TAIL = bytes([0x51, 0x04, 0xFD, 0x77])


def cmd(opcode, params=b""):
    return struct.pack("<HB", opcode, len(params)) + params


def cc(opcode, rp):
    params = b"\x01" + struct.pack("<H", opcode) + rp
    return bytes([0x0E, len(params)]) + params


def vendor(params):
    return bytes([0xFF, len(params)]) + params


def record(tag, data=b""):
    return bytes([tag]) + struct.pack(">H", len(data)) + data


def event(packet):
    return record(0, packet)


# ---------------------------------------------------------------- Intel

def intel_body():
    """Boot parameters + NOP, three big commands, small ones: all groups 4-aligned."""
    out = cmd(0xFC0E, struct.pack("<IBBB", 0x100800, 9, 9, 9))
    out += cmd(0xFC8E, bytes(3))
    for i, n in enumerate((253, 253, 249)):
        out += cmd(0xFC0B, bytes([0x50 + i]) * n)
    out += cmd(0xFC8E, b"\x01\x02\x03") + cmd(0xFC8E, b"\x04\x05\x06") + cmd(0xFC8E, b"\x00")
    return out


def intel_sfi(ecdsa, body):
    rsa = bytearray((i % 251) ^ 0x40 for i in range(644))
    rsa[8:12] = struct.pack("<I", 0x00010000)
    out = bytes(rsa)
    if ecdsa:
        block = bytearray((i % 241) ^ 0x20 for i in range(320))
        block[0] = 0x06
        block[8:12] = struct.pack("<I", 0x00020000)
        out += bytes(block)
    return out + body


def chunks(n):
    return (n + 251) // 252


def intel_fragment_count(body):
    """Secure_Send commands for a RSA-engine download: 1 + 2 + 2 header pieces,
    then each aligned group of whole commands in 252-octet pieces."""
    total = chunks(128) + chunks(256) + chunks(256)
    pos = group = 0
    while pos < len(body):
        end = pos + 3 + body[pos + 2]
        group += end - pos
        pos = end
        if group % 4 == 0:
            total += chunks(group)
            group = 0
    assert group == 0
    return total


def tlv(ty, value):
    return bytes([ty, len(value)]) + value


def tlv_version(hw_variant, img_type, sbe):
    out = b"\x00"
    out += tlv(0x10, struct.pack("<I", 0x410)) + tlv(0x11, struct.pack("<I", 0x410))
    out += tlv(0x12, struct.pack("<I", 0x37 << 8 | hw_variant << 16))
    out += tlv(0x50, b"skipped") + tlv(0x1C, bytes([img_type])) + tlv(0x2F, bytes([sbe]))
    out += tlv(0x2D, bytes([24, 29, 26]))
    return out


DDC = bytes([0x03, 0x28, 0x01, 0x18, 0x04, 0x29, 0x01, 0x03, 0x00])
LEGACY_VERSION = bytes([0, 0x37, 0x13, 0, 0x06, 4, 0, 0, 0, 0])


def intel_load_script():
    body = intel_body()
    out = bytes([0]) + event(cc(0xFC05, tlv_version(0x19, 1, 0)))
    out += record(1, intel_sfi(True, body))
    out += event(cc(0xFC09, b"\x00")) * intel_fragment_count(body)
    out += event(vendor(bytes([0x06, 0, 0x09, 0xFC, 0])))
    out += event(cc(0xFC01, b"\x00")) + event(vendor(bytes([0x02, 0, 5, 1, 0, 0, 1])))
    out += record(1, DDC)
    out += event(cc(0xFC8B, b"\x00")) * 2
    return out


def intel_refused_script():
    body = intel_body()
    out = bytes([0]) + event(cc(0xFC05, tlv_version(0x19, 1, 1)))
    out += record(1, intel_sfi(True, body))
    out += event(cc(0xFC09, b"\x00")) * 2 + event(cc(0xFC09, b"\x12"))
    out += event(cc(0xFC01, b"\x00"))
    return out


def intel_timeout_script():
    body = intel_body()
    out = bytes([0]) + event(cc(0xFC05, tlv_version(0x19, 1, 0)))
    out += record(1, intel_sfi(False, body)) + record(3, b"\x00") + record(3, b"\xff")
    out += record(4)
    return out


# ------------------------------------------------------------- Realtek

def epatch_v1(patch, fw_version, project):
    out = b"Realtech" + struct.pack("<IH", fw_version, 1)
    out += struct.pack("<HHI", 1, len(patch) + 4, 14 + 8)
    out += patch + b"\xEE" * 4
    return out + bytes([project, 1, 0]) + TAIL


def epatch_v2(project):
    sub = struct.pack("<BBBBI", 1, 1, 0, 0, 3) + b"abc"
    section = struct.pack("<HH", 1, 0) + sub
    out = b"RTBTCore" + bytes(8) + struct.pack("<I", 1)
    out += struct.pack("<II", 1, len(section)) + section
    return out + bytes([project, 1, 0]) + TAIL


def rtl_version(hci_ver, rev, lmp_sub):
    return bytes([0, hci_ver]) + struct.pack("<HBHH", rev, hci_ver, 93, lmp_sub)


RTL8761BU = rtl_version(0xA, 0xB, 0x8761)


def realtek_load_script():
    patch = bytes(i % 256 for i in range(300))
    fw = epatch_v1(patch, 0x01020304, 14)
    payload = len(patch) + 4
    out = bytes([1]) + event(cc(0x1001, RTL8761BU))
    out += event(cc(0xFC6D, b"\x00\x00")) + event(cc(0xFC61, b"\x00\x00\x00"))
    out += record(1, fw) + record(2)
    out += event(cc(0xFC20, b"\x00\x00")) * (payload // 252 + 1)
    out += event(cc(0x1001, RTL8761BU))
    return out


def realtek_unknown_script():
    unknown = rtl_version(9, 0x1234, 0x4321)
    return (bytes([1]) + event(cc(0x1001, unknown)) + record(3, b"\x02")
            + event(cc(0x1001, unknown)))


def seeds():
    body = intel_body()
    return {
        "empty": b"",
        "sfi_rsa": intel_sfi(False, body),
        "sfi_rsa_and_ecdsa": intel_sfi(True, body),
        "sfi_truncated": intel_sfi(True, body)[:900],
        "version_tlv": tlv_version(0x19, 1, 0)[1:],
        "version_legacy": LEGACY_VERSION,
        "ddc": DDC,
        "ddc_cut": DDC[:-2],
        "epatch_v1": epatch_v1(bytes(range(40)), 7, 14),
        "epatch_v2": epatch_v2(13),
        "epatch_v1_bad_length": epatch_v1(bytes(range(40)), 7, 14)[:18] + b"\xff\xff"
        + epatch_v1(bytes(range(40)), 7, 14)[20:],
        "script_intel_load": intel_load_script(),
        "script_intel_refused": intel_refused_script(),
        "script_intel_timeout": intel_timeout_script(),
        "script_realtek_load": realtek_load_script(),
        "script_realtek_unknown": realtek_unknown_script(),
    }
