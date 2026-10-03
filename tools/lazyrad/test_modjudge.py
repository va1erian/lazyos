"""The MOD player recording judge fails when it should (`modjudge.py`)."""

from __future__ import annotations

import random
import struct
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import modjudge  # noqa: E402
from modjudge import Audio, judge, read_wav  # noqa: E402

RATE = 8000


def song(seconds: float = 6.0, seed: int = 3) -> list[float]:
    """A deterministic stand-in for music: noise bursts with a beat."""
    rng = random.Random(seed)
    out = []
    for i in range(int(seconds * RATE)):
        beat = 1.0 if (i // (RATE // 4)) % 2 == 0 else 0.4
        out.append(rng.uniform(-6000, 6000) * beat)
    return out


def wav(frames: list[tuple[int, int]], rate: int, sizes: bool = True) -> bytes:
    body = b"".join(struct.pack("<hh", left, right) for left, right in frames)
    fmt = struct.pack("<HHIIHH", 1, 2, rate, rate * 4, 4, 16)
    data_size = len(body) if sizes else 0
    riff = 4 + 8 + len(fmt) + 8 + len(body) if sizes else 0
    return (b"RIFF" + struct.pack("<I", riff) + b"WAVE" + b"fmt " + struct.pack("<I", len(fmt))
            + fmt + b"data" + struct.pack("<I", data_size) + body)


class JudgeTests(unittest.TestCase):
    def reference(self) -> Audio:
        return Audio(RATE, song())

    def test_the_same_song_passes_even_quieter_and_shifted(self) -> None:
        rec = [0.0] * 300 + [0.8 * x for x in song()]
        self.assertTrue(judge(Audio(RATE, rec), self.reference()).ok)

    def test_a_repeated_stretch_fails(self) -> None:
        music = song()
        cut = 3 * RATE
        rec = music[:cut] + music[cut - RATE // 16:]  # 62 ms played twice
        verdict = judge(Audio(RATE, rec), self.reference())
        self.assertFalse(verdict.ok, verdict.lines)

    def test_a_skipped_stretch_fails(self) -> None:
        music = song()
        rec = music[:3 * RATE] + music[3 * RATE + RATE // 16:]
        self.assertFalse(judge(Audio(RATE, rec), self.reference()).ok)

    def test_silence_and_a_short_recording_fail(self) -> None:
        self.assertFalse(judge(Audio(RATE, [0.0] * len(song())), self.reference()).ok)
        self.assertFalse(judge(Audio(RATE, song()[:2 * RATE]), self.reference()).ok)

    def test_another_song_fails(self) -> None:
        self.assertFalse(judge(Audio(RATE, song(seed=9)), self.reference()).ok)

    def test_the_reader_sums_channels_and_tolerates_zero_sizes(self) -> None:
        frames = [(100, -40), (7, 8)]
        for sizes in (True, False):
            audio = read_wav(wav(frames, 22050, sizes))
            self.assertEqual(audio.rate, 22050)
            self.assertEqual(audio.mono, [60.0, 15.0])
        with self.assertRaises(ValueError):
            read_wav(b"nope")

    def test_a_different_rate_is_compared_after_resampling(self) -> None:
        music = song()
        doubled = [x for x in music for _ in (0, 1)]
        self.assertTrue(judge(Audio(2 * RATE, doubled), self.reference()).ok)
        self.assertEqual(modjudge.resample(Audio(2 * RATE, [1, 2, 3, 4]), RATE), [1, 3])


if __name__ == "__main__":
    unittest.main()
