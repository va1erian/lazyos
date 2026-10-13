#!/usr/bin/env python3
"""Tests of the write-only-when-changed helpers (`tools/outfile.py`).

    python tools/test_outfile.py
"""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import outfile  # noqa: E402


class OutfileTests(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.dir = Path(tmp.name)

    def aged(self, path: Path) -> int:
        """Set `path` to a time well in the past and return it."""
        old = 1_000_000_000_000_000_000
        os.utime(path, ns=(old, old))
        return old

    def test_the_same_bytes_leave_the_file_and_its_time_alone(self) -> None:
        source, dest = self.dir / "a", self.dir / "b"
        source.write_bytes(b"program" * 1000)
        dest.write_bytes(b"program" * 1000)
        old = self.aged(dest)
        self.assertFalse(outfile.copy_if_changed(source, dest))
        self.assertEqual(dest.stat().st_mtime_ns, old)

    def test_other_bytes_are_copied(self) -> None:
        source, dest = self.dir / "a", self.dir / "b"
        source.write_bytes(b"new")
        dest.write_bytes(b"old")
        old = self.aged(dest)
        self.assertTrue(outfile.copy_if_changed(source, dest))
        self.assertEqual(dest.read_bytes(), b"new")
        self.assertNotEqual(dest.stat().st_mtime_ns, old)

    def test_a_missing_destination_and_its_folders_are_made(self) -> None:
        source = self.dir / "a"
        source.write_bytes(b"x")
        dest = self.dir / "deep" / "er" / "b"
        self.assertTrue(outfile.copy_if_changed(source, dest))
        self.assertEqual(dest.read_bytes(), b"x")

    def test_a_same_sized_different_file_is_noticed(self) -> None:
        source, dest = self.dir / "a", self.dir / "b"
        source.write_bytes(b"A" * (3 << 20))
        dest.write_bytes(b"A" * ((3 << 20) - 1) + b"B")
        self.assertTrue(outfile.copy_if_changed(source, dest))
        self.assertEqual(dest.read_bytes(), source.read_bytes())

    def test_write_if_changed(self) -> None:
        dest = self.dir / "out" / "f"
        self.assertTrue(outfile.write_if_changed(dest, b"one"))
        old = self.aged(dest)
        self.assertFalse(outfile.write_if_changed(dest, b"one"))
        self.assertEqual(dest.stat().st_mtime_ns, old)
        self.assertTrue(outfile.write_if_changed(dest, b"two"))
        self.assertEqual(dest.read_bytes(), b"two")
        self.assertEqual([p.name for p in dest.parent.iterdir()], ["f"], "no temp file left")

    def test_copy_tree_makes_the_same_folder(self) -> None:
        source = self.dir / "src"
        (source / "icons").mkdir(parents=True)
        (source / "empty").mkdir()
        (source / "manifest.toml").write_text("x = 1\n")
        (source / "icons" / "a.png").write_bytes(b"\x89PNG")
        dest = self.dir / "dst"
        outfile.copy_tree(source, dest)
        self.assertEqual(sorted(p.relative_to(dest).as_posix() for p in dest.rglob("*")),
                         ["empty", "icons", "icons/a.png", "manifest.toml"])
        self.assertEqual((dest / "icons" / "a.png").read_bytes(), b"\x89PNG")
        with self.assertRaises(FileExistsError):
            outfile.copy_tree(source, dest)


if __name__ == "__main__":
    unittest.main()
