"""Tests for `sample.py`: the register parser and the summary."""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import sample  # noqa: E402

KERNEL = """CPU#0
RAX=0000000000000016 RBX=ffff800000316840
RIP=ffff80000007fe6e RFL=00000297 [--S-APC] CPL=0 II=0 A20=1 SMM=0 HLT=0
CR0=80010013 CR2=000000000069f000 CR3=0000000005f04000 CR4=00040620
"""
USER = KERNEL.replace("ffff80000007fe6e", "0000000000799488").replace("CPL=0", "CPL=3")


class ParseTests(unittest.TestCase):
    def test_a_kernel_snapshot(self):
        self.assertEqual(sample.parse_registers(KERNEL), (0, 0x5F04000, 0xFFFF80000007FE6E))

    def test_a_user_snapshot(self):
        self.assertEqual(sample.parse_registers(USER), (3, 0x5F04000, 0x799488))

    def test_a_snapshot_with_a_missing_field_is_refused(self):
        self.assertIsNone(sample.parse_registers(KERNEL.replace("CR3=", "CRX=")))
        self.assertIsNone(sample.parse_registers(""))


class SummaryTests(unittest.TestCase):
    def rows(self):
        idle = [(i * 0.03, 0, 0x5F04000, 0xFFFF80000007FE6E) for i in range(8)]
        busy = [(1.0 + i * 0.03, 3, 0x14B10000, 0x1234) for i in range(2)]
        return idle + busy

    def test_the_busiest_space_is_first_and_percentages_add_up(self):
        lines = sample.summarize(self.rows())
        self.assertIn("10 samples", lines[0])
        self.assertIn("cpl=0 cr3=5f04000: 8 (80%)", lines[1])
        self.assertIn("cpl=3 cr3=14b10000: 2 (20%)", lines[2])

    def test_one_process_lists_only_its_own_addresses(self):
        lines = sample.summarize(self.rows(), cr3=0x14B10000)
        self.assertTrue(any("0000000000001234  2" in line for line in lines))
        self.assertFalse(any("ffff80000007fe6e" in line for line in lines))

    def test_an_empty_recording_says_so(self):
        self.assertEqual(sample.summarize([]), ["no samples"])

    def test_load_skips_lines_that_are_not_samples(self):
        import tempfile

        with tempfile.TemporaryDirectory() as scratch:
            path = Path(scratch) / "s.tsv"
            path.write_text("0.030\t3\t0000000014b10000\t0000000000001234\nnot a sample\n", encoding="utf-8")
            self.assertEqual(sample.load(path), [(0.03, 3, 0x14B10000, 0x1234)])


if __name__ == "__main__":
    unittest.main()
