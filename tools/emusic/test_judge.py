#!/usr/bin/env python3
"""The emusic sound judge passes a correct recording and fails broken ones:
a synthetic recording of the sound-check sequence, then one fault at a time."""

from __future__ import annotations

import math
import struct
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(HERE.parent / "sound"))
import judge  # noqa: E402

RATE = 22_050


def tone(freq: float, ms: float, amplitude: float = 0.4) -> list[int]:
    count = int(RATE * ms / 1000)
    return [int(amplitude * 32767 * math.sin(2 * math.pi * freq * i / RATE)) for i in range(count)]


def silence(ms: float) -> list[int]:
    return [0] * int(RATE * ms / 1000)


def wav(samples: list[int]) -> bytes:
    data = struct.pack(f"<{len(samples)}h", *samples)
    header = struct.pack("<4sI4s4sIHHIIHH4sI", b"RIFF", 36 + len(data), b"WAVE", b"fmt ", 16,
                         1, 1, RATE, RATE * 2, 2, 16, b"data", len(data))
    return header + data


def recording(seek_ms: float = 500, paused_a4: float = 2000, quiet: float = 0.2) -> list[int]:
    """The sound check as QEMU records it: no silence between the phases."""
    a4, c5 = judge.A4, judge.C5
    return (silence(500) + tone(a4, 2000) + tone(c5, 2000)
            + tone(a4, seek_ms) + tone(c5, 2000)
            + tone(a4, paused_a4) + tone(c5, 2000)
            + tone(a4, 2000, quiet) + tone(c5, 2000, quiet) + silence(500))


def failures(samples: list[int]) -> list[str]:
    return judge.judge_wav(wav(samples))[1]


class JudgeTests(unittest.TestCase):
    def test_a_correct_recording_passes(self) -> None:
        self.assertEqual(failures(recording()), [])

    def test_a_seek_that_missed_fails(self) -> None:
        self.assertTrue(failures(recording(seek_ms=1000)))

    def test_a_pause_that_lost_audio_fails(self) -> None:
        self.assertTrue(any("segment 5" in f for f in failures(recording(paused_a4=1600))))

    def test_a_pause_that_repeated_audio_fails(self) -> None:
        self.assertTrue(any("segment 5" in f for f in failures(recording(paused_a4=2400))))

    def test_a_volume_that_did_not_apply_fails(self) -> None:
        self.assertTrue(any("volume" in f for f in failures(recording(quiet=0.4))))

    def test_the_wrong_tone_fails(self) -> None:
        samples = recording()
        samples[RATE // 2:RATE // 2 + RATE] = tone(660.0, 1000)
        self.assertTrue(failures(samples))

    def test_silence_fails(self) -> None:
        self.assertTrue(failures(silence(5000)))


if __name__ == "__main__":
    unittest.main()
