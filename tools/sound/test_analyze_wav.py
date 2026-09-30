#!/usr/bin/env python3
"""Unit tests for the WAV detector: the harness must fail when it should."""

from __future__ import annotations

import math
import struct
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import analyze_wav as aw  # noqa: E402

RATE = 44100


def tone(freq: float, ms: int, amp: int = 16000, rate: int = RATE) -> list[int]:
    count = rate * ms // 1000
    return [int(amp * math.sin(2 * math.pi * freq * i / rate)) for i in range(count)]


def silence(ms: int, rate: int = RATE) -> list[int]:
    return [0] * (rate * ms // 1000)


def wav_bytes(samples: list[int], rate: int = RATE, channels: int = 2, bits: int = 16,
              patch_sizes: bool = True) -> bytes:
    """A PCM WAV of `samples` duplicated across `channels`."""
    if bits == 16:
        frames = b"".join(struct.pack("<h", s) * channels for s in samples)
    else:
        frames = b"".join(struct.pack("<i", s << 16) * channels for s in samples)
    size = len(frames) if patch_sizes else 0
    header = b"RIFF" + struct.pack("<I", 36 + size if patch_sizes else 0) + b"WAVE"
    header += b"fmt " + struct.pack("<IHHIIHH", 16, 1, channels, rate,
                                    rate * channels * bits // 8, channels * bits // 8, bits)
    header += b"data" + struct.pack("<I", size)
    return header + frames


def run(samples, **kwargs):
    return aw.analyze(aw.read_wav(wav_bytes(samples, **kwargs)))


class Reader(unittest.TestCase):
    def test_reads_a_patched_file(self):
        wav = aw.read_wav(wav_bytes(tone(440, 100)))
        self.assertEqual((wav.rate, wav.channels, wav.bits), (RATE, 2, 16))
        self.assertEqual(len(wav.samples), RATE // 10)

    def test_tolerates_unpatched_sizes_from_a_killed_emulator(self):
        wav = aw.read_wav(wav_bytes(tone(440, 100), patch_sizes=False))
        self.assertEqual(len(wav.samples), RATE // 10)

    def test_reads_32_bit_samples_on_the_16_bit_scale(self):
        wav = aw.read_wav(wav_bytes(tone(440, 100), bits=32))
        self.assertLess(abs(max(wav.samples) - 16000), 50)

    def test_rejects_garbage(self):
        for bad in (b"", b"RIFF", b"not a wav file at all", b"RIFF\0\0\0\0WAVE"):
            with self.assertRaises(aw.WavError):
                aw.read_wav(bad)

    def test_rejects_non_pcm(self):
        data = bytearray(wav_bytes(tone(440, 50)))
        data[20:22] = struct.pack("<H", 3)  # IEEE float
        with self.assertRaises(aw.WavError):
            aw.read_wav(bytes(data))


class Detector(unittest.TestCase):
    def test_single_tone_frequency_and_length(self):
        report = run(tone(440, 1000))
        self.assertEqual(len(report.segments), 1)
        segment = report.segments[0]
        self.assertAlmostEqual(segment.freq_hz, 440, delta=4)
        self.assertGreater(segment.duration_ms, 900)
        self.assertGreater(segment.peak, 15000)

    def test_two_tones_back_to_back_are_two_segments(self):
        report = run(tone(440, 800) + tone(880, 800))
        self.assertEqual(len(report.segments), 2)
        self.assertAlmostEqual(report.segments[0].freq_hz, 440, delta=5)
        self.assertAlmostEqual(report.segments[1].freq_hz, 880, delta=9)

    def test_gap_between_tones_is_fine_too(self):
        report = run(tone(440, 500) + silence(300) + tone(880, 500))
        self.assertEqual([round(s.freq_hz / 10) * 10 for s in report.segments], [440, 880])

    def test_silence_has_no_segments(self):
        report = run(silence(1000))
        self.assertEqual(report.segments, [])
        self.assertEqual(report.peak, 0)

    def test_quiet_noise_is_not_a_tone(self):
        report = run([(-1) ** i * 100 for i in range(RATE)])
        self.assertEqual(report.segments, [])

    def test_a_short_click_is_ignored(self):
        report = run(silence(200) + tone(1000, 30) + silence(200))
        self.assertEqual(report.segments, [])

    def test_other_rates(self):
        report = run(tone(440, 600, rate=48000), rate=48000)
        self.assertAlmostEqual(report.segments[0].freq_hz, 440, delta=5)


class Verdict(unittest.TestCase):
    def check(self, samples, freqs, **kw):
        kw.setdefault("tolerance", 0.02)
        kw.setdefault("min_ms", 300.0)
        kw.setdefault("min_peak", 4000)
        return aw.check(run(samples), freqs, **kw)

    def test_the_expected_pair_passes(self):
        self.assertEqual(self.check(tone(440, 800) + tone(880, 800), [440, 880]), [])

    def test_silence_fails(self):
        self.assertTrue(self.check(silence(1000), [440]))

    def test_wrong_pitch_fails(self):
        failures = self.check(tone(523, 800), [440])
        self.assertTrue(any("frequency" in f for f in failures), failures)

    def test_missing_second_tone_fails(self):
        failures = self.check(tone(440, 800), [440, 880])
        self.assertTrue(any("expected 2" in f for f in failures), failures)

    def test_swapped_order_fails(self):
        self.assertTrue(self.check(tone(880, 800) + tone(440, 800), [440, 880]))

    def test_too_short_fails(self):
        failures = self.check(tone(440, 400), [440], min_ms=1000)
        self.assertTrue(any("ms below" in f for f in failures), failures)

    def test_too_quiet_fails(self):
        failures = self.check(tone(440, 800, amp=1500), [440])
        self.assertTrue(any("peak" in f for f in failures), failures)

    def test_no_expectation_only_needs_some_tone(self):
        self.assertEqual(self.check(tone(300, 500), []), [])
        self.assertTrue(self.check(silence(500), []))


class Cli(unittest.TestCase):
    def test_exit_codes(self):
        import tempfile
        with tempfile.TemporaryDirectory() as tmp:
            good = Path(tmp) / "good.wav"
            good.write_bytes(wav_bytes(tone(440, 800) + tone(880, 800)))
            bad = Path(tmp) / "bad.wav"
            bad.write_bytes(wav_bytes(silence(800)))
            self.assertEqual(aw.main([str(good), "--expect-freq", "440,880"]), 0)
            self.assertEqual(aw.main([str(bad), "--expect-freq", "440,880"]), 1)
            self.assertEqual(aw.main([str(Path(tmp) / "missing.wav")]), 1)


if __name__ == "__main__":
    unittest.main()
