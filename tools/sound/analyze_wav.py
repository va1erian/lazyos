#!/usr/bin/env python3
"""Measure a WAV recording: which tones are in it, how long, how loud, what pitch.

QEMU's `wav` audio backend writes the guest's output to a file. This module is
the detector behind `tools/sound/run.py`. It splits the recording into *tone
segments* (runs of sound at a steady pitch) and reports, for each, the
duration, peak, RMS and the frequency from rising zero crossings. Two tones
played back to back with no silence between them are still two segments,
because segmentation follows the pitch, not the gaps.

The reader is deliberately tolerant. QEMU patches the RIFF and `data` sizes
into the header only when it shuts down cleanly; a recording from a killed
emulator has zero or bogus sizes, so the data chunk runs to end of file when
its declared size is 0, larger than the file, or `0xFFFFFFFF`.

Usage
-----
    python tools/sound/analyze_wav.py out.wav --expect-freq 440
    python tools/sound/analyze_wav.py out.wav --expect-freq 440,880 --min-ms 640
"""

from __future__ import annotations

import argparse
import math
import struct
import sys
from dataclasses import dataclass
from pathlib import Path

#: A sample above this (16-bit scale) counts as sound, not the noise floor.
ACTIVE_THRESHOLD = 512
#: Pitch analysis window.
WINDOW_MS = 40
#: Windows whose pitch differs by more than this fraction start a new segment.
PITCH_SPLIT = 0.15
#: Segments shorter than this many windows are clicks or boundary blends.
MIN_WINDOWS = 3


@dataclass
class Wav:
    rate: int
    channels: int
    bits: int
    #: Channel 0 samples as signed integers (16-bit scale).
    samples: list[int]


@dataclass
class Segment:
    start_ms: float
    end_ms: float
    peak: int
    rms: float
    freq_hz: float | None
    crossings: int

    @property
    def duration_ms(self) -> float:
        return self.end_ms - self.start_ms


@dataclass
class Report:
    rate: int
    channels: int
    total_ms: float
    peak: int
    segments: list[Segment]


class WavError(Exception):
    pass


def read_wav(data: bytes) -> Wav:
    """Parse a PCM WAV, tolerating unpatched (0 / oversized) size fields."""
    if len(data) < 12 or data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        raise WavError("not a RIFF/WAVE file")
    fmt = None
    pos = 12
    while pos + 8 <= len(data):
        tag = data[pos:pos + 4]
        size = struct.unpack_from("<I", data, pos + 4)[0]
        body = pos + 8
        if tag == b"fmt ":
            if size < 16 or body + 16 > len(data):
                raise WavError("truncated fmt chunk")
            tag_, channels, rate, _byte_rate, _align, bits = struct.unpack_from(
                "<HHIIHH", data, body
            )
            if tag_ not in (1, 0xFFFE):
                raise WavError(f"not PCM (format tag {tag_})")
            fmt = (channels, rate, bits)
        elif tag == b"data":
            if fmt is None:
                raise WavError("data chunk before fmt")
            end = body + size
            if size == 0 or size == 0xFFFFFFFF or end > len(data):
                end = len(data)
            channels, rate, bits = fmt
            return Wav(rate, channels, bits, decode(data[body:end], channels, bits))
        pos = body + size + (size & 1)
    raise WavError("no data chunk")


def decode(raw: bytes, channels: int, bits: int) -> list[int]:
    """Channel 0 of interleaved little-endian PCM as signed ints (16-bit scale)."""
    if channels < 1:
        raise WavError("zero channels")
    if bits == 16:
        frame = 2 * channels
        return [struct.unpack_from("<h", raw, i * frame)[0] for i in range(len(raw) // frame)]
    if bits == 32:
        frame = 4 * channels
        return [struct.unpack_from("<i", raw, i * frame)[0] >> 16 for i in range(len(raw) // frame)]
    if bits == 8:
        return [(raw[i * channels] - 128) << 8 for i in range(len(raw) // channels)]
    raise WavError(f"unsupported sample width {bits}")


def rising_crossings(samples: list[int]) -> list[int]:
    """Indices where the signal crosses zero going up."""
    return [i for i in range(1, len(samples)) if samples[i - 1] < 0 <= samples[i]]


def frequency(samples: list[int], rate: int) -> tuple[float | None, int]:
    """Frequency from the span between first and last rising crossing."""
    crossings = rising_crossings(samples)
    if len(crossings) < 2:
        return None, len(crossings)
    span = crossings[-1] - crossings[0]
    return ((len(crossings) - 1) * rate / span if span else None), len(crossings)


def window_pitch(window: list[int], rate: int) -> float | None:
    """Coarse pitch of one analysis window, or None when it is silent."""
    if max((abs(s) for s in window), default=0) <= ACTIVE_THRESHOLD:
        return None
    return len(rising_crossings(window)) * 1000.0 / WINDOW_MS


def find_segments(wav: Wav) -> list[Segment]:
    """Group consecutive windows of similar pitch into tone segments."""
    size = max(1, wav.rate * WINDOW_MS // 1000)
    windows = [wav.samples[i:i + size] for i in range(0, len(wav.samples) - size + 1, size)]
    pitches = [window_pitch(w, wav.rate) for w in windows]

    groups: list[list[int]] = []
    current: list[int] = []
    reference = 0.0
    for index, pitch in enumerate(pitches):
        if pitch is None or pitch == 0:
            if current:
                groups.append(current)
            current, reference = [], 0.0
        elif current and abs(pitch - reference) <= PITCH_SPLIT * reference:
            current.append(index)
        else:
            if current:
                groups.append(current)
            current, reference = [index], pitch
    if current:
        groups.append(current)

    segments = []
    for group in groups:
        if len(group) < MIN_WINDOWS:
            continue
        # Measure the interior: the first and last windows may blend two tones.
        body = group[1:-1] if len(group) > 2 else group
        span = [s for i in body for s in windows[i]]
        full = [s for i in group for s in windows[i]]
        freq, crossings = frequency(span, wav.rate)
        segments.append(
            Segment(
                start_ms=1000.0 * group[0] * size / wav.rate,
                end_ms=1000.0 * (group[-1] + 1) * size / wav.rate,
                peak=max(abs(s) for s in full),
                rms=math.sqrt(sum(s * s for s in full) / len(full)),
                freq_hz=freq,
                crossings=crossings,
            )
        )
    return segments


def analyze(wav: Wav) -> Report:
    return Report(
        rate=wav.rate,
        channels=wav.channels,
        total_ms=1000.0 * len(wav.samples) / wav.rate,
        peak=max((abs(s) for s in wav.samples), default=0),
        segments=find_segments(wav),
    )


def check(
    report: Report,
    expect_freqs: list[float],
    tolerance: float,
    min_ms: float,
    min_peak: int,
) -> list[str]:
    """Human-readable failures (empty when the recording passes).

    With `expect_freqs` the recording must contain that many segments, in that
    order, each at its frequency; without, it must simply not be silent.
    """
    failures = []
    if report.peak < min_peak:
        failures.append(f"peak {report.peak} below {min_peak}: silence or too quiet")
    if not expect_freqs:
        if not report.segments:
            failures.append("no tone found")
        return failures
    if len(report.segments) != len(expect_freqs):
        failures.append(
            f"found {len(report.segments)} tone segment(s), expected {len(expect_freqs)}"
        )
    for index, (segment, wanted) in enumerate(zip(report.segments, expect_freqs)):
        label = f"segment {index + 1}"
        if segment.peak < min_peak:
            failures.append(f"{label}: peak {segment.peak} below {min_peak}")
        if segment.duration_ms < min_ms:
            failures.append(f"{label}: {segment.duration_ms:.0f} ms below {min_ms:.0f} ms")
        if segment.freq_hz is None:
            failures.append(f"{label}: no frequency measurable")
        elif abs(segment.freq_hz - wanted) > wanted * tolerance:
            failures.append(
                f"{label}: frequency {segment.freq_hz:.1f} Hz is not {wanted:g} Hz "
                f"(+/-{tolerance * 100:g}%)"
            )
    return failures


def parse_freqs(text: str | None) -> list[float]:
    return [float(part) for part in text.split(",") if part] if text else []


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("wav", type=Path)
    parser.add_argument("--expect-freq", help="expected tone frequencies in Hz, in order, comma separated")
    parser.add_argument("--tolerance", type=float, default=0.02, help="allowed relative error (default 0.02)")
    parser.add_argument("--min-ms", type=float, default=300.0, help="minimum duration of each segment")
    parser.add_argument("--min-peak", type=int, default=4000, help="minimum peak (16-bit scale)")
    args = parser.parse_args(argv)

    try:
        report = analyze(read_wav(args.wav.read_bytes()))
    except (OSError, WavError) as exc:
        print(f"SOUND:WAV:FAIL {exc}")
        return 1
    print(
        f"SOUND:WAV rate={report.rate} channels={report.channels} "
        f"total_ms={report.total_ms:.0f} peak={report.peak} segments={len(report.segments)}"
    )
    for index, segment in enumerate(report.segments, 1):
        freq = f"{segment.freq_hz:.1f}" if segment.freq_hz is not None else "none"
        print(
            f"SOUND:TONE {index} start_ms={segment.start_ms:.0f} "
            f"duration_ms={segment.duration_ms:.0f} peak={segment.peak} "
            f"rms={segment.rms:.0f} freq_hz={freq}"
        )
    failures = check(report, parse_freqs(args.expect_freq), args.tolerance, args.min_ms, args.min_peak)
    for failure in failures:
        print(f"SOUND:WAV:FAIL {failure}")
    if not failures:
        print("SOUND:WAV:PASS")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
