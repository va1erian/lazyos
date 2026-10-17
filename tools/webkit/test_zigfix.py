#!/usr/bin/env python3
"""zigfix.py: the patch applies once, refuses an allocator it does not know, and names stale programs."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "jsc"))
import zigfix  # noqa: E402
from toolenv import BuildError  # noqa: E402

SOURCE = f"fn getCpuCount() u32 {{\n    {zigfix.ANCHOR}\n    return n;\n}}\n"


class ZigFix(unittest.TestCase):
    def test_the_slot_count_has_a_floor_and_a_ceiling(self):
        fixed = zigfix.patched(SOURCE)
        self.assertIn("@max(std.Thread.getCpuCount()", fixed)
        self.assertIn(f"{zigfix.MIN_SLOTS}), max_thread_count)", fixed)
        self.assertLessEqual(zigfix.MIN_SLOTS, 128, "zig's own maximum")

    def test_it_applies_once_and_is_idempotent(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "SmpAllocator.zig"
            path.write_text(SOURCE)
            self.assertTrue(zigfix.apply(path))
            once = path.read_text()
            self.assertFalse(zigfix.apply(path))
            self.assertEqual(path.read_text(), once)
            self.assertNotIn(zigfix.ANCHOR, once)

    def test_an_unknown_allocator_is_an_error_not_a_silent_skip(self):
        with self.assertRaises(BuildError):
            zigfix.patched("fn getCpuCount() u32 { return 1; }")

    def test_programs_are_stale_until_linked_with_this_version(self):
        with tempfile.TemporaryDirectory() as tmp:
            record = Path(tmp) / "record.json"
            self.assertEqual(zigfix.stale(record, ["a", "b"]), ["a", "b"])
            zigfix.mark(record, ["a"])
            self.assertEqual(zigfix.stale(record, ["a", "b"]), ["b"])
            record.write_text('{"a": "0"}')
            self.assertEqual(zigfix.stale(record, ["a"]), ["a"], "an older version of the fix")
            record.write_text("not json")
            self.assertEqual(zigfix.stale(record, ["a"]), ["a"])


if __name__ == "__main__":
    unittest.main()
