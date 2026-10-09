#!/usr/bin/env python3
"""The stick pre-flight checker's parsers (no image needed).

    python tools/boot/test_preflight.py
"""

from __future__ import annotations

import struct
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import preflight  # noqa: E402


def mbr(entries):
    sector = bytearray(512)
    for i, (boot, ptype, start, count) in enumerate(entries):
        sector[446 + 16 * i : 462 + 16 * i] = (
            bytes([boot, 0, 0, 0, ptype, 0, 0, 0]) + struct.pack("<II", start, count)
        )
    sector[510:512] = b"\x55\xaa"
    return bytes(sector)


def pe(machine=0x8664, magic=0x20B, subsystem=10):
    data = bytearray(512)
    data[0:2] = b"MZ"
    data[0x3C:0x40] = struct.pack("<I", 0x80)
    data[0x80:0x84] = b"PE\0\0"
    data[0x84:0x86] = struct.pack("<H", machine)
    data[0x98:0x9A] = struct.pack("<H", magic)
    data[0x98 + 68 : 0x98 + 70] = struct.pack("<H", subsystem)
    return bytes(data)


class Mem:
    def __init__(self, data):
        self.data, self.size = data, len(data)

    def read(self, offset, length):
        return self.data[offset : offset + length]


class Parsers(unittest.TestCase):
    def test_mbr_entries_skip_empty_slots(self):
        parts = preflight.mbr_entries(mbr([(0x80, 0x0C, 2048, 4096), (0, 0x83, 8192, 100)]))
        self.assertEqual([(p["type"], p["active"], p["start"]) for p in parts],
                         [(0x0C, True, 2048), (0x83, False, 8192)])

    def check_pe(self, **kw):
        report = preflight.Report()
        preflight.check_pe(pe(**kw), report)
        return report.rows[0]["ok"]

    def test_pe_accepts_x64_efi_application(self):
        self.assertTrue(self.check_pe())

    def test_pe_refuses_other_machine_subsystem_or_32_bit(self):
        self.assertFalse(self.check_pe(machine=0x014C))
        self.assertFalse(self.check_pe(subsystem=3))
        self.assertFalse(self.check_pe(magic=0x10B))

    def test_pe_refuses_garbage(self):
        report = preflight.Report()
        preflight.check_pe(b"\0" * 512, report)
        self.assertFalse(report.rows[0]["ok"])

    def test_ext2_label(self):
        sb = bytearray(2048)
        sb[1024 + 56 : 1024 + 58] = struct.pack("<H", 0xEF53)
        sb[1024 + 120 : 1024 + 128] = b"lazyhome"
        self.assertEqual(preflight.ext2_label(Mem(bytes(sb)), 0), (True, "lazyhome"))
        self.assertEqual(preflight.ext2_label(Mem(bytes(2048)), 0)[0], False)

    def test_a_missing_partition_table_fails_without_crashing(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "image.bin"
            path.write_bytes(b"\0" * 512)
            report, image = preflight.run(path, 0)
            image.f.close()
        self.assertTrue(any(not row["ok"] for row in report.rows))

    def test_a_zeroed_boot_sector_is_a_failed_check_not_a_traceback(self):
        data = bytearray(mbr([(0x80, 0x20, 1, 100), (0x80, 0x0C, 2048, 4096), (0, 0x83, 8192, 100)]))
        data += bytes(2048 * 512 - len(data) + 4096 * 512)  # zeroed sectors under the FAT
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "image.bin"
            path.write_bytes(bytes(data))
            report, image = preflight.run(path, 0)
            image.f.close()
        failed = [row["check"] for row in report.rows if not row["ok"]]
        self.assertIn("image structures are readable", failed)


if __name__ == "__main__":
    unittest.main()
