#!/usr/bin/env python3
"""Judge a recording of the *mixer*: tones that must sound together, and levels.

`analyze_wav.py` follows one pitch at a time (zero crossings), which is exactly
what a chord defeats. This detector measures, per short window, how loud each
expected frequency is (a Hann-windowed Goertzel filter per frequency), labels
each window with the set of frequencies present, and checks the labels come
in the expected order and last long enough. Levels are compared between
steps, so "half volume" means "about half the amplitude of the reference
tone", whatever the backend's scaling.

    python tools/sound/mixcheck.py out.wav --expect 440,660+990,880 \\
        --min-ms 600 --level 880/440=0.5

`--expect` lists the steps in order; `+` joins frequencies that must sound at
the same time. `--level A/B=R` requires tone A's amplitude to be R times tone
B's (within `--level-tolerance`). Each frequency inside a chord must also be
about as loud as it would be alone (mixing adds, it does not attenuate).
"""

from __future__ import annotations

import argparse
import math
import sys
from dataclasses import dataclass
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import analyze_wav  # noqa: E402

#: Analysis window.
WINDOW_MS = 40
#: A frequency counts as present above this fraction of the loudest
#: frequency's amplitude anywhere in the recording.
PRESENT_FRACTION = 0.2
#: Runs shorter than this many windows are boundary blends, not steps.
MIN_WINDOWS = 3


@dataclass
class Step:
    """A run of windows with the same set of frequencies present."""

    freqs: frozenset[float]
    start_ms: float
    end_ms: float
    #: Median amplitude of each present frequency over the run.
    levels: dict[float, float]

    @property
    def duration_ms(self) -> float:
        return self.end_ms - self.start_ms

    def label(self) -> str:
        return "+".join(f"{f:g}" for f in sorted(self.freqs)) or "silence"


def goertzel_amplitude(window: list[float], rate: int, freq: float) -> float:
    """Amplitude of `freq` in a Hann-windowed block (a full-scale sine of
    amplitude A measures about A)."""
    n = len(window)
    coeff = 2.0 * math.cos(2.0 * math.pi * freq / rate)
    s1 = s2 = 0.0
    for sample in window:
        s1, s2 = sample + coeff * s1 - s2, s1
    power = max(s1 * s1 + s2 * s2 - coeff * s1 * s2, 0.0)
    # A Hann window halves a sine's coherent gain.
    return 4.0 * math.sqrt(power) / n


def window_levels(wav: analyze_wav.Wav, freqs: list[float]) -> list[dict[float, float]]:
    """Per window, the amplitude of every frequency."""
    size = max(16, wav.rate * WINDOW_MS // 1000)
    hann = [0.5 - 0.5 * math.cos(2 * math.pi * i / (size - 1)) for i in range(size)]
    levels = []
    for start in range(0, len(wav.samples) - size + 1, size):
        block = [s * w for s, w in zip(wav.samples[start:start + size], hann)]
        levels.append({f: goertzel_amplitude(block, wav.rate, f) for f in freqs})
    return levels


def median(values: list[float]) -> float:
    ordered = sorted(values)
    return ordered[len(ordered) // 2] if ordered else 0.0


def find_steps(wav: analyze_wav.Wav, freqs: list[float]) -> list[Step]:
    """Label each window with the frequencies present and merge runs."""
    levels = window_levels(wav, freqs)
    loudest = max((max(w.values()) for w in levels), default=0.0)
    if loudest <= 0:
        return []
    floor = loudest * PRESENT_FRACTION
    labels = [frozenset(f for f, a in w.items() if a >= floor) for w in levels]
    steps: list[Step] = []
    start = 0
    for index in range(1, len(labels) + 1):
        if index < len(labels) and labels[index] == labels[start]:
            continue
        if labels[start] and index - start >= MIN_WINDOWS:
            run = levels[start:index]
            steps.append(Step(
                labels[start],
                start * WINDOW_MS,
                index * WINDOW_MS,
                {f: median([w[f] for w in run]) for f in labels[start]},
            ))
        start = index
    # A run broken by a blended window is still one step.
    merged: list[Step] = []
    for step in steps:
        if merged and merged[-1].freqs == step.freqs:
            previous = merged[-1]
            previous.end_ms = step.end_ms
            continue
        merged.append(step)
    return merged


def parse_expect(text: str) -> list[frozenset[float]]:
    return [frozenset(float(f) for f in step.split("+")) for step in text.split(",") if step]


def parse_level(text: str) -> tuple[float, float, float]:
    pair, ratio = text.split("=")
    a, b = pair.split("/")
    return float(a), float(b), float(ratio)


def judge(
    steps: list[Step],
    expect: list[frozenset[float]],
    min_ms: float,
    levels: list[tuple[float, float, float]],
    tolerance: float,
) -> list[str]:
    """Every problem found (empty when the recording passes)."""
    problems = []
    found = [s.freqs for s in steps]
    if found != expect:
        problems.append(
            "expected steps "
            + ", ".join("+".join(f"{f:g}" for f in sorted(e)) for e in expect)
            + "; found "
            + (", ".join(s.label() for s in steps) or "nothing")
        )
        return problems
    for step in steps:
        if step.duration_ms < min_ms:
            problems.append(f"{step.label()} lasted {step.duration_ms:.0f} ms (< {min_ms:.0f})")
    alone = {next(iter(s.freqs)): s.levels[next(iter(s.freqs))] for s in steps if len(s.freqs) == 1}
    reference = max(alone.values(), default=0.0)
    for step in steps:
        if len(step.freqs) < 2 or reference <= 0:
            continue
        for freq in step.freqs:
            share = step.levels[freq] / reference
            if abs(share - 1.0) > tolerance:
                problems.append(
                    f"{freq:g} Hz in the chord {step.label()} is at {share:.2f} of a lone tone"
                )
    for a, b, ratio in levels:
        if a not in alone or b not in alone or alone[b] <= 0:
            problems.append(f"no lone {a:g} Hz / {b:g} Hz step to compare")
            continue
        measured = alone[a] / alone[b]
        if abs(measured - ratio) > tolerance * ratio:
            problems.append(f"{a:g} Hz is {measured:.2f} of {b:g} Hz, expected {ratio:g}")
    return problems


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("wav")
    parser.add_argument("--expect", required=True, help="steps in order, e.g. 440,660+990,880")
    parser.add_argument("--min-ms", type=float, default=500.0)
    parser.add_argument("--level", action="append", default=[], help="A/B=ratio of lone-tone amplitudes")
    parser.add_argument("--level-tolerance", type=float, default=0.2)
    args = parser.parse_args(argv)
    try:
        wav = analyze_wav.read_wav(Path(args.wav).read_bytes())
    except (OSError, analyze_wav.WavError) as error:
        print(f"MIX:FAIL cannot read {args.wav}: {error}")
        return 1
    expect = parse_expect(args.expect)
    freqs = sorted({f for step in expect for f in step})
    steps = find_steps(wav, freqs)
    for step in steps:
        levels = " ".join(f"{f:g}Hz={a:.0f}" for f, a in sorted(step.levels.items()))
        print(f"  step {step.label():<12} {step.start_ms:7.0f}-{step.end_ms:7.0f} ms  {levels}")
    problems = judge(steps, expect, args.min_ms, [parse_level(l) for l in args.level],
                     args.level_tolerance)
    for problem in problems:
        print(f"MIX:FAIL {problem}")
    if not problems:
        print("MIX:PASS")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
