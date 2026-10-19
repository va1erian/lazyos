#!/usr/bin/env python3
"""Self-test of tools/nes/validate_external.py: the judge passes what jsnes
reports for a good cartridge and fails each way a broken one fails, and — when
this host has Node and npm — the real check runs on the shipped sample."""

from __future__ import annotations

import shutil
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import validate_external  # noqa: E402


def good() -> dict:
    """What jsnes reports for the shipped cartridge (measured)."""
    return {
        "frames": 218,
        "frames_after_note": 29,
        "colours": 4,
        "common_pixels": 55_000,
        "total_pixels": 61_440,
        "audio_samples": 24_000,
        "peak": 0.05,
        "note_hz": 440,
        "joy": 0x01,
        "moved": True,
        "sprite_rows": 15,
        "sprite_columns": 16,
    }


class Judge(unittest.TestCase):
    def test_a_good_run_passes(self):
        self.assertEqual(validate_external.judge(good(), 200), [])

    def test_a_stuck_console_fails(self):
        report = good() | {"frames": 3, "frames_after_note": 0}
        failures = validate_external.judge(report, 200)
        self.assertTrue(any("counted" in f for f in failures), failures)

    def test_a_missing_dpad_fails(self):
        report = good() | {"joy": 0x00, "moved": False}
        failures = validate_external.judge(report, 200)
        self.assertTrue(any("Right held" in f for f in failures), failures)
        self.assertTrue(any("ball" in f for f in failures), failures)

    def test_a_blank_picture_fails(self):
        report = good() | {"colours": 1, "common_pixels": 61_440}
        failures = validate_external.judge(report, 200)
        self.assertTrue(any("colours" in f for f in failures), failures)

    def test_a_wrong_note_fails(self):
        report = good() | {"note_hz": 660}
        failures = validate_external.judge(report, 200)
        self.assertTrue(any("note" in f for f in failures), failures)

    def test_a_half_drawn_ball_fails(self):
        # The OAMADDR quirk: the ball's two top sprites eaten by the
        # pre-render copy.
        report = good() | {"sprite_rows": 7}
        failures = validate_external.judge(report, 200)
        self.assertTrue(any("16x15" in f for f in failures), failures)

    def test_silence_fails(self):
        report = good() | {"peak": 0.0, "audio_samples": 10}
        failures = validate_external.judge(report, 200)
        self.assertTrue(any("silent" in f for f in failures), failures)
        self.assertTrue(any("samples" in f for f in failures), failures)


@unittest.skipUnless(
    shutil.which("node") and shutil.which("npm"), "no node/npm on this host"
)
class AgainstJsnes(unittest.TestCase):
    def test_the_shipped_sample_runs_in_jsnes(self):
        self.assertEqual(
            validate_external.main(["--frames", "200"]),
            0,
            "the shipped cartridge did not pass in jsnes",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
