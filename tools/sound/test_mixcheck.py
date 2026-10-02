#!/usr/bin/env python3
"""Unit tests for the mixer detector: it must fail when it should."""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import mixcheck  # noqa: E402
from test_analyze_wav import silence, tone, wav_bytes  # noqa: E402

EXPECT = ["--expect", "440,660+990,880", "--min-ms", "600", "--level", "880/440=0.5"]


def chord(freqs: list[float], ms: int, amp: int = 16000) -> list[int]:
    parts = [tone(f, ms, amp) for f in freqs]
    return [sum(samples) for samples in zip(*parts)]


def verdict(samples: list[int], args: list[str] = EXPECT) -> int:
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "mix.wav"
        clipped = [max(-32768, min(32767, s)) for s in samples]
        path.write_bytes(wav_bytes(clipped))
        return mixcheck.main([str(path), *args])


def good() -> list[int]:
    return (
        tone(440, 800)
        + silence(200)
        + chord([660, 990], 1500)
        + silence(200)
        + tone(880, 800, amp=8000)
    )


class Mix(unittest.TestCase):
    def test_the_expected_recording_passes(self):
        self.assertEqual(verdict(good()), 0)

    def test_tones_one_after_another_are_not_a_chord(self):
        samples = tone(440, 800) + tone(660, 750) + tone(990, 750) + tone(880, 800, amp=8000)
        self.assertEqual(verdict(samples), 1)

    def test_a_missing_chord_partner_fails(self):
        samples = tone(440, 800) + chord([660], 1500) + tone(880, 800, amp=8000)
        self.assertEqual(verdict(samples), 1)

    def test_full_volume_instead_of_half_fails(self):
        samples = tone(440, 800) + chord([660, 990], 1500) + tone(880, 800)
        self.assertEqual(verdict(samples), 1)

    def test_an_attenuated_mix_fails(self):
        # A "mixer" that halves each stream to avoid clipping is not unity.
        samples = tone(440, 800) + chord([660, 990], 1500, amp=8000) + tone(880, 800, amp=8000)
        self.assertEqual(verdict(samples), 1)

    def test_a_short_chord_fails(self):
        samples = tone(440, 800) + chord([660, 990], 300) + tone(880, 800, amp=8000)
        self.assertEqual(verdict(samples), 1)

    def test_silence_and_garbage_fail(self):
        self.assertEqual(verdict(silence(3000)), 1)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bad.wav"
            path.write_bytes(b"not a wav")
            self.assertEqual(mixcheck.main([str(path), *EXPECT]), 1)

    def test_steps_report_levels(self):
        from analyze_wav import read_wav

        steps = mixcheck.find_steps(read_wav(wav_bytes(good())), [440, 660, 880, 990])
        self.assertEqual([s.label() for s in steps], ["440", "660+990", "880"])
        half = steps[2].levels[880] / steps[0].levels[440]
        self.assertAlmostEqual(half, 0.5, delta=0.05)


if __name__ == "__main__":
    unittest.main()
