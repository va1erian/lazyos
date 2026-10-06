#!/usr/bin/env python3
"""The probe judge on a NIC that pads short frames (`--nic e1000`, issue #497).

An Intel 8254x pads every frame shorter than 60 bytes on the wire, so the
probe's 14-byte legal extreme arrives as 60 bytes: the 14 it sent and 46
zeros. The judge must accept exactly that, and still catch a dropped extreme,
a damaged one, and a 13-byte frame that leaked out padded.
"""

from __future__ import annotations

import contextlib
import io
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import analyze_pcap  # noqa: E402
import pcap  # noqa: E402
from test_analyze_pcap import analyze, probe, reply, request, text  # noqa: E402


def padded(frame: bytes) -> bytes:
    return frame + bytes(max(0, 60 - len(frame)))


class PaddedProbe(unittest.TestCase):
    base = [request(), reply()]

    def test_a_padded_short_extreme_passes(self):
        report = analyze(self.base + [padded(probe(14)), probe(1514)], expect_probe=True, padded=True)
        self.assertTrue(report.ok, text(report))
        self.assertIn("lengths on the wire=[60, 1514]", text(report))

    def test_unpadded_frames_still_pass_in_padded_mode(self):
        report = analyze(self.base + [probe(14), probe(1514)], expect_probe=True, padded=True)
        self.assertTrue(report.ok, text(report))

    def test_padding_is_not_accepted_unless_the_nic_pads(self):
        report = analyze(self.base + [padded(probe(14)), probe(1514)], expect_probe=True)
        self.assertFalse(report.ok)

    def test_a_padded_extreme_with_nonzero_padding_fails(self):
        damaged = bytearray(padded(probe(14)))
        damaged[40] = 0x77
        report = analyze(self.base + [bytes(damaged), probe(1514)], expect_probe=True, padded=True)
        self.assertFalse(report.ok)
        self.assertIn("payload differs", text(report))

    def test_a_dropped_short_extreme_fails(self):
        report = analyze(self.base + [probe(1514)], expect_probe=True, padded=True)
        self.assertFalse(report.ok)
        self.assertIn("no 14-byte probe frame", text(report))

    def test_a_padded_runt_leaking_fails(self):
        report = analyze(self.base + [padded(probe(14)), probe(1514), padded(probe(13))],
                         expect_probe=True, padded=True)
        self.assertFalse(report.ok)
        self.assertIn("13-byte frame reached the wire", text(report))

    def test_the_command_line_judges_a_padded_capture_with_padded(self):
        frames = self.base + [padded(probe(14)), probe(1514)]
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "net.pcap"
            path.write_bytes(pcap.write_pcap(frames))
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(analyze_pcap.main([str(path), "--expect-probe", "--padded"]), 0)
                self.assertEqual(analyze_pcap.main([str(path), "--expect-probe"]), 1)


if __name__ == "__main__":
    unittest.main()
