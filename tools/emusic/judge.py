#!/usr/bin/env python3
"""Judge the recording of ``emusic.elf --sound-check`` (docs/media-plan.md P4).

The sound check plays ``tones.mp3`` (A4 for 2 s, then C5 for 2 s) four times
(``emusic/src/soundcheck.rs``). QEMU's WAV recorder skips the time no stream
plays, so the silences between phases and during the pause are not in the
recording; every phase starts on A4 and ends on C5, so the phases still split
by pitch. The recording must hold, in order, the tone segments

====== =========================================================
phase  segments
====== =========================================================
whole  A4 2 s, C5 2 s
seek   A4 0.5 s, C5 2 s (the seek to 1.5 s landed on its sample)
pause  A4 2 s, C5 2 s (the pause at 1 s lost and repeated nothing)
volume A4 2 s, C5 2 s, each 6 dB below phase 1's
====== =========================================================

Frequencies within 3 %, durations within 150 ms, the level within 1.5 dB.
Segments are found by labelling each 20 ms window A4, C5 or silent from its
energy at the two frequencies (Goertzel), which tells two tones a minor third
apart reliably where a zero-crossing count per window cannot.

    python tools/emusic/judge.py shots/emusic_sound/emusic.wav
"""

from __future__ import annotations

import argparse
import math
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "sound"))
from analyze_wav import frequency, read_wav  # noqa: E402

A4, C5 = 440.0, 523.25
FREQ_TOLERANCE = 0.03
DURATION_TOLERANCE_MS = 150.0
LEVEL_TOLERANCE_DB = 1.5
WINDOW_MS = 20
#: A window quieter than this fraction of the loudest sample is silent.
SILENCE = 0.1
#: Runs shorter than this many windows are boundary blends.
MIN_WINDOWS = 3


@dataclass
class Expected:
    phase: str
    freq: float
    ms: float


@dataclass
class Segment:
    tone: float
    start_ms: float
    duration_ms: float
    freq_hz: float
    rms: float


#: Every segment the recording must hold, in order.
EXPECTED = [
    Expected("whole", A4, 2000), Expected("whole", C5, 2000),
    Expected("seek", A4, 500), Expected("seek", C5, 2000),
    Expected("pause", A4, 2000), Expected("pause", C5, 2000),
    Expected("volume", A4, 2000), Expected("volume", C5, 2000),
]


def goertzel(window: list[int], rate: int, freq: float) -> float:
    """The power of `window` at `freq`."""
    coefficient = 2 * math.cos(2 * math.pi * freq / rate)
    previous = before = 0.0
    for sample in window:
        previous, before = sample + coefficient * previous - before, previous
    return previous * previous + before * before - coefficient * previous * before


def label(window: list[int], rate: int, floor: float) -> float | None:
    """The tone `window` holds, or None when it is silent."""
    if max((abs(s) for s in window), default=0) <= floor:
        return None
    return A4 if goertzel(window, rate, A4) >= goertzel(window, rate, C5) else C5


def segments(samples: list[int], rate: int) -> list[Segment]:
    """The runs of one tone in `samples`."""
    size = max(1, rate * WINDOW_MS // 1000)
    windows = [samples[i:i + size] for i in range(0, len(samples) - size + 1, size)]
    floor = SILENCE * max((abs(s) for s in samples), default=0)
    labels = [label(window, rate, floor) for window in windows]
    found = []
    start = 0
    for index in range(1, len(labels) + 1):
        if index < len(labels) and labels[index] == labels[start]:
            continue
        tone, count = labels[start], index - start
        if tone is not None and count >= MIN_WINDOWS:
            span = [s for window in windows[start:index] for s in window]
            # The interior, away from the blends at either end.
            body = [s for window in windows[start + 1:index - 1] for s in window] or span
            found.append(Segment(
                tone=tone,
                start_ms=start * WINDOW_MS,
                duration_ms=count * WINDOW_MS,
                freq_hz=frequency(body, rate)[0] or 0.0,
                rms=math.sqrt(sum(s * s for s in span) / len(span)),
            ))
        start = index
    # A blend too short to keep can split one tone: join neighbours of a tone.
    joined: list[Segment] = []
    for segment in found:
        last = joined[-1] if joined else None
        if last and last.tone == segment.tone:
            last.duration_ms = segment.start_ms + segment.duration_ms - last.start_ms
        else:
            joined.append(segment)
    return joined


def judge(found: list[Segment]) -> list[str]:
    """What is wrong with `found`; empty when the recording passes."""
    if len(found) != len(EXPECTED):
        listing = ", ".join(f"{s.freq_hz:.0f} Hz/{s.duration_ms:.0f} ms" for s in found)
        return [f"expected {len(EXPECTED)} tone segments, found {len(found)}: {listing}"]
    failures = []
    for index, (segment, expected) in enumerate(zip(found, EXPECTED)):
        where = f"segment {index + 1} ({expected.phase})"
        if abs(segment.freq_hz - expected.freq) > expected.freq * FREQ_TOLERANCE:
            failures.append(f"{where}: {segment.freq_hz:.1f} Hz, expected {expected.freq} Hz")
        if abs(segment.duration_ms - expected.ms) > DURATION_TOLERANCE_MS:
            failures.append(f"{where}: {segment.duration_ms:.0f} ms, expected {expected.ms:.0f} ms")
    for loud, quiet in ((0, 6), (1, 7)):
        ratio = found[quiet].rms / max(found[loud].rms, 1e-9)
        db = 20 * math.log10(max(ratio, 1e-9))
        if abs(db + 6.02) > LEVEL_TOLERANCE_DB:
            failures.append(f"volume: segment {quiet + 1} is {db:+.1f} dB, expected -6 dB")
    return failures


def judge_wav(data: bytes) -> tuple[list[Segment], list[str]]:
    """The segments of a WAV file and what is wrong with them."""
    wav = read_wav(data)
    found = segments(wav.samples, wav.rate)
    return found, judge(found)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("wav", type=Path)
    args = parser.parse_args(argv)
    found, failures = judge_wav(args.wav.read_bytes())
    for segment in found:
        print(f"  {segment.start_ms:8.0f} ms  {segment.duration_ms:6.0f} ms  "
              f"{segment.freq_hz:7.1f} Hz  rms {segment.rms:7.0f}")
    for failure in failures:
        print(f"EMUSIC:JUDGE:FAIL:{failure}")
    if failures:
        return 1
    print("EMUSIC:JUDGE:PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
