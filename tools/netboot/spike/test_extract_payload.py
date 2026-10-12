"""extract_payload.py against a synthetic stick (no real image needed)."""

import hashlib
import json
import tempfile
import unittest
from pathlib import Path

import common
import extract_payload
import fat16_format
import testlib


class ExtractTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.contents = testlib.make_payload(self.dir / "src")
        self.stick = self.dir / "stick.img"
        testlib.make_stick(self.stick, self.dir / "src")

    def tearDown(self):
        self.tmp.cleanup()

    def test_four_files_come_out_intact(self):
        out = self.dir / "out"
        rows = extract_payload.extract(self.stick, out)
        self.assertEqual([r["name"] for r in rows], ["kernel-x86_64", "boot.json", "ramdisk", "bootx64.efi"])
        for row in rows:
            data = self.contents[row["name"]]
            self.assertEqual((out / row["name"]).read_bytes(), data)
            self.assertEqual(row["size"], len(data))
            self.assertEqual(row["sha256"], hashlib.sha256(data).hexdigest())

    def test_main_writes_manifest_and_checks_the_loader(self):
        out = self.dir / "out"
        self.assertEqual(extract_payload.main(["--image", str(self.stick), "--out", str(out)]), 0)
        manifest = json.loads((out / "manifest.json").read_text())
        self.assertEqual({r["name"] for r in manifest}, {"kernel-x86_64", "boot.json", "ramdisk", "bootx64.efi"})
        self.assertTrue(all(set(r) == {"name", "size", "sha256"} for r in manifest))

    def test_a_loader_that_is_not_pe_fails_main(self):
        bad = self.dir / "bad"
        testlib.make_payload(bad)
        (bad / common.LOADER_NAME).write_bytes(b"not an efi application" * 10)
        stick = self.dir / "bad.img"
        testlib.make_stick(stick, bad)
        self.assertEqual(extract_payload.main(["--image", str(stick), "--out", str(self.dir / "o2")]), 1)

    def test_missing_file_is_an_error(self):
        files = {common.LOADER_FAT_PATH: self.dir / "src" / common.LOADER_NAME,
                 "kernel-x86_64": self.dir / "src" / "kernel-x86_64"}
        stick = self.dir / "partial.img"
        fat16_format.format_image(stick, files, part_type=0x0C)
        with self.assertRaises(extract_payload.ExtractError):
            extract_payload.extract(stick, self.dir / "o3")

    def test_image_without_a_fat_partition_is_an_error(self):
        blank = self.dir / "blank.img"
        blank.write_bytes(b"\0" * 4096)
        with self.assertRaises(extract_payload.ExtractError):
            extract_payload.extract(blank, self.dir / "o4")

    def test_missing_image_exit_code(self):
        self.assertEqual(extract_payload.main(["--image", str(self.dir / "nope.img"),
                                               "--out", str(self.dir / "o5")]), 2)


if __name__ == "__main__":
    unittest.main()
