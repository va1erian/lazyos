#!/usr/bin/env python3
"""The Volume judge fails when it should: synthetic recordings."""

from __future__ import annotations

import math
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import volume_check  # noqa: E402
from volume_check import analyze_wav  # noqa: E402

RATE = 48_000


def tone(freq: float, ms: int, amplitude: float) -> list[int]:
    count = RATE * ms // 1000
    return [int(amplitude * math.sin(2 * math.pi * freq * i / RATE)) for i in range(count)]


def silence(ms: int) -> list[int]:
    return [0] * (RATE * ms // 1000)


def recording(first: float, second: float) -> analyze_wav.Wav:
    samples = silence(200) + tone(440, 800, first) + silence(400) + tone(660, 800, second) + silence(200)
    return analyze_wav.Wav(rate=RATE, channels=1, bits=16, samples=samples)


class JudgeTests(unittest.TestCase):
    def test_half_the_level_passes(self) -> None:
        ok, detail = volume_check.judge(recording(16000, 8000), 440, 660, 0.5)
        self.assertTrue(ok, detail)

    def test_an_unchanged_level_fails(self) -> None:
        ok, _ = volume_check.judge(recording(16000, 16000), 440, 660, 0.5)
        self.assertFalse(ok)

    def test_a_missing_tone_fails(self) -> None:
        wav = analyze_wav.Wav(rate=RATE, channels=1, bits=16,
                              samples=silence(200) + tone(440, 800, 16000) + silence(200))
        ok, detail = volume_check.judge(wav, 440, 660, 0.5)
        self.assertFalse(ok)
        self.assertIn("missing", detail)


if __name__ == "__main__":
    unittest.main()
