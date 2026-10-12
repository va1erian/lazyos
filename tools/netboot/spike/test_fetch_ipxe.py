"""fetch_ipxe.py's rules, with a local file standing in for the download."""

import datetime
import tempfile
import unittest
from pathlib import Path

import common
import fetch_ipxe
import testlib

NOW = datetime.datetime(2026, 10, 12, 9, 30, tzinfo=datetime.timezone.utc)


class RuleTests(unittest.TestCase):
    def test_decide(self):
        self.assertEqual(fetch_ipxe.decide(None, "a", False), "write")
        self.assertEqual(fetch_ipxe.decide("a", "a", False), "keep")
        self.assertEqual(fetch_ipxe.decide("a", "b", True), "replace")
        with self.assertRaises(fetch_ipxe.FetchError):
            fetch_ipxe.decide("a", "b", False)

    def test_pe_check(self):
        fetch_ipxe.check_efi_binary(testlib.fake_efi())
        for bad in (b"", b"MZ" + b"\0" * 100, b"#!/bin/sh\n" * 50, testlib.fake_efi()[:0x30]):
            with self.assertRaises(fetch_ipxe.FetchError):
                fetch_ipxe.check_efi_binary(bad)

    def test_i386_pe_is_refused(self):
        data = bytearray(testlib.fake_efi())
        data[0x44:0x46] = (0x14C).to_bytes(2, "little")
        with self.assertRaises(fetch_ipxe.FetchError):
            fetch_ipxe.check_efi_binary(bytes(data))

    def test_digest_record(self):
        text = fetch_ipxe.digest_record("ab" * 32, "https://x/ipxe.efi", NOW)
        self.assertEqual(text.splitlines()[0], "ab" * 32 + "  ipxe.efi")
        self.assertIn("2026-10-12 09:30:00Z", text)
        self.assertIn("https://x/ipxe.efi", text)


class DirectoryTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.source = self.dir / "src.efi"
        self.source.write_bytes(testlib.fake_efi(700))
        self.out = self.dir / "ipxe"

    def tearDown(self):
        self.tmp.cleanup()

    def fetch(self, force=False, source=None):
        return fetch_ipxe.fetch(self.out, "unused://", force, source or self.source, NOW)

    def test_first_fetch_writes_binary_and_digest(self):
        action, digest = self.fetch()
        self.assertEqual(action, "write")
        self.assertEqual(digest, common.sha256_file(self.out / "ipxe.efi"))
        self.assertTrue((self.out / "ipxe.sha256").read_text().startswith(digest))

    def test_same_bytes_are_kept(self):
        self.fetch()
        stamp = (self.out / "ipxe.sha256").read_text()
        self.assertEqual(self.fetch()[0], "keep")
        self.assertEqual((self.out / "ipxe.sha256").read_text(), stamp)

    def test_keep_writes_a_missing_record_and_never_rewrites_one(self):
        self.fetch()
        record = self.out / "ipxe.sha256"
        record.unlink()
        self.assertEqual(self.fetch()[0], "keep")
        self.assertTrue(record.read_text().startswith(common.sha256_file(self.out / "ipxe.efi")))
        record.write_text("hand-edited record\n")
        self.assertEqual(self.fetch()[0], "keep")
        self.assertEqual(record.read_text(), "hand-edited record\n")

    def test_different_bytes_need_force(self):
        _, first = self.fetch()
        other = self.dir / "other.efi"
        other.write_bytes(testlib.fake_efi(800))
        with self.assertRaises(fetch_ipxe.FetchError):
            self.fetch(source=other)
        self.assertEqual(common.sha256_file(self.out / "ipxe.efi"), first)
        self.assertEqual(self.fetch(force=True, source=other)[0], "replace")
        self.assertNotEqual(common.sha256_file(self.out / "ipxe.efi"), first)

    def test_directory_with_other_files_is_refused(self):
        self.out.mkdir()
        (self.out / "stray.txt").write_text("x")
        with self.assertRaises(fetch_ipxe.FetchError):
            self.fetch()

    def test_not_a_pe_is_not_stored(self):
        junk = self.dir / "junk"
        junk.write_bytes(b"<html>404</html>" * 20)
        with self.assertRaises(fetch_ipxe.FetchError):
            self.fetch(source=junk)
        self.assertFalse((self.out / "ipxe.efi").exists())

    def test_templates(self):
        self.fetch()
        names = fetch_ipxe.write_templates(self.out, "10.0.2.2", 8420, "192.168.1.5")
        self.assertEqual(sorted(names), ["autoexec.box.ipxe", "autoexec.ipxe", "boot.ipxe.example"])
        self.assertIn("http://10.0.2.2:8420/boot.ipxe", (self.out / "autoexec.ipxe").read_text())
        self.assertIn("http://192.168.1.5:8420/boot.ipxe", (self.out / "autoexec.box.ipxe").read_text())
        self.assertNotIn("\r", (self.out / "autoexec.ipxe").read_text())
        self.assertEqual(fetch_ipxe.write_templates(self.out, "10.0.2.2", 8420, None)[-1], "boot.ipxe.example")

    def test_main_from_file(self):
        code = fetch_ipxe.main(["--out", str(self.out), "--from-file", str(self.source)])
        self.assertEqual(code, 0)
        self.assertTrue((self.out / "autoexec.ipxe").is_file())


if __name__ == "__main__":
    unittest.main()
