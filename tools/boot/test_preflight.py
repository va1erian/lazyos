#!/usr/bin/env python3
"""The stick pre-flight checker's parsers (no image needed).

    python tools/boot/test_preflight.py
"""

from __future__ import annotations

import contextlib
import hashlib
import io
import json
import struct
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import preflight  # noqa: E402
import preflight_netboot  # noqa: E402


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


ID = "20261012-093000-01234567"
REV = "0123456789abcdef0123456789abcdef01234567"


def netboot_dir(root, mutate=None, signed=True, image_id=ID):
    """A `target/netboot` the way the build writes it; `mutate(manifest)` edits the manifest
    before it is written (digests are computed from the files first)."""
    files = {
        "kernel-x86_64": b"\x7fELF\x02\x01\x01" + bytes(9) + struct.pack("<H", 2) + struct.pack("<H", 0x3E) + bytes(100),
        "ramdisk": bytes(range(256)) * 40,
        "bootx64.efi": pe(),
        "boot.json": b'{\n  "frame_buffer": {}\n}',
    }
    for name, data in files.items():
        (root / name).write_bytes(data)
    manifest = {
        "id": ID,
        "revision": REV,
        "dirty": False,
        "built_at": "2026-10-12T09:30:00Z",
        "host": "devbox",
        "switches": ["LAZYOS_DESKTOP", "LAZYOS_USB"],
        "files": {
            name: {"size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
            for name, data in files.items()
        },
    }
    if mutate:
        mutate(manifest)
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    if signed:
        (root / "manifest.sig").write_bytes(b"ab" * 64 + b"\n")
    return image_id


class Netboot(unittest.TestCase):
    """`--netboot DIR`: the manifest against the files (the ramdisk's own structure is checked
    on a real build, so the structure check is replaced by its result here)."""

    def judge(self, mutate=None, signed=True, image_id=ID, damage=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            netboot_dir(root, mutate, signed)
            if damage:
                damage(root)
            cfg = f"root=UUID=x\nhome=LABEL=lazyhome\nimage.id={image_id}\n" if image_id else "root=UUID=x\n"
            with mock.patch.object(preflight_netboot, "check_ramdisk", lambda rimg, r, size_note="": cfg):
                return preflight_netboot.run(root)

    def failed(self, report):
        return [row["check"] for row in report.rows if not row["ok"]]

    def test_a_good_payload_passes(self):
        report = self.judge()
        self.assertEqual(self.failed(report), [])
        self.assertFalse(any(row.get("warn") for row in report.rows))

    def test_an_unsigned_payload_passes_with_a_warning(self):
        report = self.judge(signed=False)
        self.assertEqual(self.failed(report), [])
        self.assertTrue(any(row.get("warn") and "manifest.sig" in row["check"] for row in report.rows))

    def test_a_malformed_signature_fails(self):
        for text in ("ab" * 63 + "\n", "AB" * 64 + "\n", "ab" * 64, "zz" * 64 + "\n"):
            def damage(root, text=text):
                (root / "manifest.sig").write_bytes(text.encode())

            self.assertIn("manifest.sig is 128 lowercase hex digits and a newline",
                          self.failed(self.judge(damage=damage)), repr(text))

    def test_a_file_that_changed_after_the_manifest_fails(self):
        def damage(root):
            (root / "ramdisk").write_bytes(b"x" * 10)

        self.assertIn("ramdisk: size and sha256 match the manifest", self.failed(self.judge(damage=damage)))

    def test_same_size_different_bytes_fails_on_the_digest(self):
        def damage(root):
            data = bytearray((root / "boot.json").read_bytes())
            data[3] ^= 1
            (root / "boot.json").write_bytes(bytes(data))

        self.assertIn("boot.json: size and sha256 match the manifest", self.failed(self.judge(damage=damage)))

    def test_a_missing_file_fails(self):
        def damage(root):
            (root / "bootx64.efi").unlink()

        self.assertIn("bootx64.efi exists and is not empty", self.failed(self.judge(damage=damage)))

    def test_the_kernel_must_be_an_x86_64_elf(self):
        def damage(root):
            (root / "kernel-x86_64").write_bytes(b"MZ" + bytes(100))

        self.assertIn("kernel-x86_64 is an x86-64 ELF", self.failed(self.judge(damage=damage)))

    def test_the_image_id_must_be_in_the_config_and_match_the_manifest(self):
        self.assertIn("lazyos.cfg has image.id=<id> of 1 to 64 of [0-9A-Za-z._-]",
                      self.failed(self.judge(image_id=None)))
        self.assertIn("lazyos.cfg has image.id=<id> of 1 to 64 of [0-9A-Za-z._-]",
                      self.failed(self.judge(image_id="has space")))
        self.assertIn("the ramdisk's image.id is the manifest's id",
                      self.failed(self.judge(image_id="20261012-093000-ffffffff")))

    def test_manifest_fields_are_checked(self):
        cases = {
            "manifest id is <yyyymmdd-HHMMSS>-<rev8>[-dirty]": lambda m: m.update(id="nope"),
            "manifest revision is 40 hex digits or unknown": lambda m: m.update(revision="abc"),
            "manifest dirty is a bool that agrees with the id": lambda m: m.update(dirty=True),
            "the id's revision part is the manifest's revision": lambda m: m.update(revision="f" * 40),
            "manifest built_at is ISO 8601 UTC (2026-10-12T09:30:00Z)": lambda m: m.update(built_at="yesterday"),
            "the id's time is the manifest's built_at": lambda m: m.update(built_at="2026-10-12T09:31:00Z"),
            "manifest host is a non-empty string": lambda m: m.update(host=""),
            "manifest switches are the sorted LAZYOS_* names": lambda m: m.update(switches=["LAZYOS_USB", "LAZYOS_A"]),
            "manifest lists exactly the four payload files": lambda m: m["files"].pop("ramdisk"),
            "ramdisk: manifest entry is {size, sha256}": lambda m: m["files"]["ramdisk"].update(sha256="ABC"),
        }
        for expected, mutate in cases.items():
            self.assertIn(expected, self.failed(self.judge(mutate)), expected)

    def test_unknown_revision_and_dirty_ids(self):
        def unknown(m):
            m.update(revision="unknown", id="20261012-093000-00000000")

        self.assertEqual(self.failed(self.judge(unknown, image_id="20261012-093000-00000000")), [])

        def dirty(m):
            m.update(dirty=True, id=ID + "-dirty")

        self.assertEqual(self.failed(self.judge(dirty, image_id=ID + "-dirty")), [])

    def test_main_takes_a_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            netboot_dir(Path(directory))
            cfg = f"root=UUID=x\nimage.id={ID}\n"
            argv = ["preflight.py", "--netboot", directory]
            with mock.patch.object(preflight_netboot, "check_ramdisk", lambda rimg, r, size_note="": cfg), \
                    mock.patch.object(sys, "argv", argv), contextlib.redirect_stdout(io.StringIO()) as out:
                self.assertEqual(preflight.main(), 0)
            self.assertIn("PREFLIGHT:PASS", out.getvalue())
            with mock.patch.object(sys, "argv", ["preflight.py", "--netboot", directory + "/nope"]), \
                    contextlib.redirect_stderr(io.StringIO()):
                self.assertEqual(preflight.main(), 2)


if __name__ == "__main__":
    unittest.main()
