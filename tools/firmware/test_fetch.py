#!/usr/bin/env python3
"""Tests of the firmware fetcher: the manifest's rules and the download's
(no network: `urlopen` is replaced by a fake server).

Run: python tools/firmware/test_fetch.py
"""

from __future__ import annotations

import hashlib
import io
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import fetch  # noqa: E402

BLOB = b"\x22\x88" + bytes(range(256)) * 4
LICENCE = b"Redistribution in binary form is permitted.\n"


def line(name: str, data: bytes, licence: str, switch: str = "wifi") -> str:
    digest = hashlib.sha256(data).hexdigest()
    return f"{name} | {switch} | {digest} | {len(data)} | {licence} | https://example.test/{name}"


GOOD = "\n".join([
    "# a comment",
    line("acme/fw.bin", BLOB, "acme/LICENCE.acme"),
    "",
    line("acme/LICENCE.acme", LICENCE, "acme/LICENCE.acme"),
])


class FakeServer:
    """`urlopen` over a dict of URL -> bytes, counting requests."""

    def __init__(self, files: dict[str, bytes]):
        self.files = files
        self.requests = 0

    def __call__(self, request, timeout=None):
        self.requests += 1
        url = request.full_url
        if url not in self.files:
            raise OSError(f"404 {url}")
        return io.BytesIO(self.files[url])


class ManifestTests(unittest.TestCase):
    def test_the_checked_in_manifest_parses_and_pins_the_rtw88_firmware(self) -> None:
        blobs = {blob.name: blob for blob in fetch.load_manifest()}
        fw = blobs["rtw88/rtw8822c_fw.bin"]
        self.assertEqual(fw.switch, "wifi")
        self.assertIn(fw.licence, blobs, "the licence travels with the blob")
        self.assertIn("afabaf773c4c2e2c841429933a6a084a5af4d14d", fw.url, "commit-pinned")

    def test_a_good_manifest_parses(self) -> None:
        blobs = fetch.parse_manifest(GOOD)
        self.assertEqual([b.name for b in blobs], ["acme/fw.bin", "acme/LICENCE.acme"])
        self.assertEqual(blobs[0].size, len(BLOB))

    def test_bad_lines_are_refused(self) -> None:
        good = line("acme/fw.bin", BLOB, "acme/fw.bin")
        for bad, why in [
            (good.replace("acme/fw.bin |", "../fw.bin |", 1), "vendor-relative"),
            (good.replace("acme/fw.bin |", "fw.bin |", 1), "vendor-relative"),
            (good.replace(" | wifi | ", " | wifi | xyz", 1), "digest"),
            (good.replace("https://", "http://"), "https"),
            (good.replace(f" | {len(BLOB)} | ", " | 0 | "), "size"),
            (good + " | extra", "6 fields"),
            (line("acme/fw.bin", BLOB, "acme/LICENCE"), "licence"),
            (good + "\n" + good, "twice"),
        ]:
            with self.assertRaisesRegex(ValueError, why, msg=bad):
                fetch.parse_manifest(bad)

    def test_a_switch_brings_the_licences_its_files_need(self) -> None:
        text = GOOD.replace("acme/LICENCE.acme | wifi", "acme/LICENCE.acme | other")
        blobs = fetch.parse_manifest(text)
        self.assertEqual({b.name for b in fetch.wanted(blobs, "wifi")},
                         {"acme/fw.bin", "acme/LICENCE.acme"})
        self.assertEqual(fetch.wanted(blobs, "nothing"), [])


class DownloadTests(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = tempfile.TemporaryDirectory()
        self.cache = Path(self.dir.name)
        self.blobs = fetch.parse_manifest(GOOD)
        self.server = FakeServer({b.url: data for b, data in zip(self.blobs, (BLOB, LICENCE))})
        patcher = mock.patch.object(fetch.urllib.request, "urlopen", self.server)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.addCleanup(self.dir.cleanup)

    def test_files_land_verified_and_are_then_cached(self) -> None:
        self.assertTrue(fetch.fetch(self.blobs, self.cache))
        self.assertEqual((self.cache / "acme/fw.bin").read_bytes(), BLOB)
        self.assertEqual(self.server.requests, 2)
        self.assertTrue(fetch.fetch(self.blobs, self.cache))
        self.assertEqual(self.server.requests, 2, "a verified cache is not downloaded again")

    def test_wrong_bytes_never_land(self) -> None:
        self.server.files[self.blobs[0].url] = BLOB[:-1] + b"\x00"
        with mock.patch.object(fetch, "log"):
            self.assertFalse(fetch.fetch(self.blobs, self.cache))
        self.assertFalse((self.cache / "acme/fw.bin").exists())
        self.assertFalse((self.cache / "acme/fw.bin.part").exists())

    def test_an_oversized_download_is_refused(self) -> None:
        self.server.files[self.blobs[0].url] = BLOB + b"tail"
        with mock.patch.object(fetch, "log"):
            self.assertFalse(fetch.download(self.blobs[0], self.cache / "acme/fw.bin"))

    def test_a_damaged_cache_is_replaced(self) -> None:
        dest = self.cache / "acme/fw.bin"
        dest.parent.mkdir(parents=True)
        dest.write_bytes(b"truncated")
        with mock.patch.object(fetch, "log"):
            self.assertTrue(fetch.download(self.blobs[0], dest))
        self.assertEqual(dest.read_bytes(), BLOB)

    def test_an_unreachable_file_fails_the_run(self) -> None:
        del self.server.files[self.blobs[1].url]
        with mock.patch.object(fetch, "log"):
            code = fetch.main(["--cache", str(self.cache), "--manifest", str(self.write(GOOD))])
        self.assertEqual(code, 1)

    def write(self, text: str) -> Path:
        path = self.cache / "manifest.txt"
        path.write_text(text, encoding="utf-8")
        return path


if __name__ == "__main__":
    unittest.main()
