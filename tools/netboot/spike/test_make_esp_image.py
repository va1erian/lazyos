"""The FAT16 formatter and the ESP image, read back with preflight's reader."""

import tempfile
import unittest
from pathlib import Path

import common
import fat16_format
import make_esp_image
import preflight
import testlib


def open_fat(path):
    image = preflight.Image(path)
    parts = preflight.mbr_entries(image.read(0, 512))
    return image, parts, preflight.Fat(image, parts[0]["start"] * 512)


class ShortNameTests(unittest.TestCase):
    def test_upper_case_83_is_itself(self):
        self.assertEqual(fat16_format.short_name("BOOTX64.EFI", set()), (b"BOOTX64 EFI", False))

    def test_lower_case_name_keeps_its_case_in_a_long_name(self):
        self.assertEqual(fat16_format.short_name("boot.json", set()), (b"BOOT~1  JSO", True))

    def test_long_name_gets_a_tail(self):
        self.assertEqual(fat16_format.short_name("kernel-x86_64", set()), (b"KERNEL~1   ", True))

    def test_tail_is_unique(self):
        taken = {b"KERNEL~1   "}
        self.assertEqual(fat16_format.short_name("kernel-x86_65", taken)[0], b"KERNEL~2   ")

    def test_lfn_checksums_agree(self):
        short, _ = fat16_format.short_name("kernel-x86_64", set())
        entries = fat16_format.lfn_entries("kernel-x86_64", short)
        self.assertEqual(len(entries), 1)
        self.assertEqual(entries[0][13], fat16_format.lfn_checksum(short))
        self.assertEqual(entries[0][0], 0x41)

    def test_long_names_split_in_13s_and_count_down(self):
        name = "a" * 30
        entries = fat16_format.lfn_entries(name, b"AAAAAA~1   ")
        self.assertEqual([e[0] for e in entries], [0x43, 2, 1])


class RoundTripTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.payload = self.dir / "payload"
        self.contents = testlib.make_payload(self.payload)

    def tearDown(self):
        self.tmp.cleanup()

    def test_esp_image_round_trip(self):
        out = self.dir / "esp.img"
        files = make_esp_image.layout(self.payload)
        info = fat16_format.format_image(out, files)
        self.assertEqual(out.stat().st_size, info["bytes"])
        self.assertEqual(make_esp_image.read_back(out, files), [])
        image, parts, fat = open_fat(out)
        self.assertEqual(len(parts), 1)
        self.assertEqual(parts[0]["type"], 0x0E)
        self.assertTrue(parts[0]["active"])
        self.assertEqual(parts[0]["start"], 2048)
        self.assertEqual(fat.kind, "FAT16")
        self.assertTrue(fat.signature)
        self.assertEqual(fat.label, "LAZYOSESP")
        for name, data in self.contents.items():
            dest = common.LOADER_FAT_PATH if name == common.LOADER_NAME else name
            entry = fat.find(dest)
            self.assertIsNotNone(entry, dest)
            self.assertEqual(fat.read_chain(entry[2], entry[3]), data)
        # Names keep their case (preflight reads the long-name entries).
        self.assertEqual([e[0] for e in fat.dir_entries(fat.root_cluster)][:2], ["EFI", "boot.json"])
        self.assertEqual(fat.first_entries("EFI"), [".", ".."])
        self.assertEqual(fat.first_entries("EFI/BOOT"), [".", ".."])
        image.f.close()

    def test_image_is_deterministic(self):
        files = make_esp_image.layout(self.payload)
        a, b = self.dir / "a.img", self.dir / "b.img"
        fat16_format.format_image(a, files)
        fat16_format.format_image(b, files)
        self.assertEqual(a.read_bytes(), b.read_bytes())

    def test_many_files_fill_more_than_a_cluster_of_directory(self):
        files = {}
        for i in range(70):
            source = self.dir / f"f{i}.bin"
            source.write_bytes(bytes([i]) * (i * 700))  # includes an empty file
            files[f"data/a-fairly-long-file-name-{i}.bin"] = source
        out = self.dir / "many.img"
        fat16_format.format_image(out, files)
        image, _, fat = open_fat(out)
        for dest, source in files.items():
            entry = fat.find(dest)
            self.assertIsNotNone(entry, dest)
            self.assertEqual(fat.read_chain(entry[2], entry[3]), source.read_bytes())
        image.f.close()

    def test_cluster_size_grows_with_the_payload(self):
        big = self.dir / "big.bin"
        with open(big, "wb") as f:
            f.truncate(70 << 20)
        out = self.dir / "big.img"
        info = fat16_format.format_image(out, {"big.bin": big})
        self.assertEqual(info["spc"], 4)
        self.assertLessEqual(info["clusters"], fat16_format.FAT16_MAX_CLUSTERS)
        image, _, fat = open_fat(out)
        self.assertEqual(fat.kind, "FAT16")
        self.assertEqual(fat.find("big.bin")[3], 70 << 20)
        image.f.close()

    def test_too_big_for_fat16_is_refused(self):
        big = self.dir / "huge.bin"
        with open(big, "wb") as f:
            f.truncate(2200 << 20)
        with self.assertRaises(fat16_format.FormatError):
            fat16_format.format_image(self.dir / "huge.img", {"huge.bin": big})

    def test_duplicate_path_refused(self):
        source = self.dir / "x"
        source.write_bytes(b"x")
        node = fat16_format.Node()
        node.add("a", source)
        with self.assertRaises(fat16_format.FormatError):
            node.add("a", source)

    def test_two_names_for_one_source_file_each_read_back(self):
        # the same Path object under two names must not share (or lose) a file's clusters
        source = self.dir / "shared.bin"
        source.write_bytes(bytes(range(256)) * 40)
        other = self.dir / "other.bin"
        other.write_bytes(b"other contents")
        out = self.dir / "dup.img"
        fat16_format.format_image(out, {"a.bin": source, "b.bin": source, "c.bin": other})
        image, _, fat = open_fat(out)
        for name, data in (("a.bin", source.read_bytes()), ("b.bin", source.read_bytes()),
                           ("c.bin", other.read_bytes())):
            entry = fat.find(name)
            self.assertIsNotNone(entry, name)
            self.assertEqual(fat.read_chain(entry[2], entry[3]), data, name)
        image.f.close()

    def test_read_back_notices_damage(self):
        out = self.dir / "esp.img"
        files = make_esp_image.layout(self.payload)
        fat16_format.format_image(out, files)
        image, _, fat = open_fat(out)
        entry = fat.find("ramdisk")
        offset = fat.base + (fat.first_data + (entry[2] - 2) * fat.spc) * fat.bps + 100
        image.f.close()
        with open(out, "r+b") as f:
            f.seek(offset)
            f.write(b"\xff\xff\xff\xff")
        self.assertTrue(any("ramdisk" in p for p in make_esp_image.read_back(out, files)))

    def test_missing_payload_file_is_reported(self):
        (self.payload / "ramdisk").unlink()
        with self.assertRaises(fat16_format.FormatError):
            make_esp_image.layout(self.payload)


if __name__ == "__main__":
    unittest.main()
