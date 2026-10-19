#!/usr/bin/env python3
"""Judge the NES Player's session (docs/nes-player.md).

`tools/nes/run.py` builds a desktop image with the Terminal, the UI probe and
the sound card, runs `tools/screenshot/examples/xui_nes.json` (the app is
launched on the sample cartridge from the Terminal, plays for a few seconds,
moves the ball with the D-pad, saves and loads a state, pauses and quits)
while QEMU records the sound card to a WAV, and judges:

* the serial log: the LOAD/RUN/STATE/QUIT markers, and no `NES:FAIL`;
* the recording: the sample ROM's melody (`tools/nes/sample_rom.py`);
* the screenshots: the console's own colours in `shot_01_playing.png`, the
  ball moved in `shot_02_moved.png`, and the pause overlay in
  `shot_03_paused.png`.

The judge is exercised by `tools/nes/test_judge.py`; every function returns a
list of failures, empty when the input passes.
"""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "sound"))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "screenshot"))
import analyze_wav  # noqa: E402
import pngstats  # noqa: E402

#: The markers a good run shows, in the order they appear.
REQUIRED = [
    "NES:LOAD:PASS:/system/share/samples/lazyos-sample.nes:mapper=0",
    "NES:RUN:PASS:frames=",
    "NES:STATE:PASS:save=1:",
    "NES:STATE:PASS:load=1:",
    "NES:QUIT:PASS",
    "NES:HEADLESS:PASS:frames=300",
]
#: Markers that must never appear in a good run.
FORBIDDEN = [
    "NES:FAIL",
    "NES:SOUND:FAIL",
    "NES:BIND:FAIL",
    "NES:RUN:FAIL",
    "NES:BUILD:FAIL",
    "NES:HEADLESS:FAIL",
]
#: The sample ROM's melody, in Hz (A4, C5, E5, A5).
NOTES = [440.0, 523.251, 659.255, 880.0]
#: How far a measured note may be from its own frequency.
NOTE_TOLERANCE = 0.05
#: How long a note must be heard, and how many of the four must be.
NOTE_MIN_MS = 150.0
MIN_NOTES = 3
#: The console's palette in the sample ROM: the backdrop and the text.
BACKDROP = (0x00, 0x23, 0x7B)
TEXT = (0xFF, 0xFF, 0xFF)
#: The ball's sprite colour (palette 0 of the sprites, `$3F11`), and the shape
#: it must have: 16x15 pixels, drawn by four sprites.
BALL = (0xED, 0x98, 0x1F)
BALL_ROWS = 14
#: How far the ball must have moved between the two shots (the session holds
#: the D-pad for three seconds).
BALL_MOVE = 5
#: The paused overlay's colour (`screen.rs`, `Color::hex(0xFF_D0_50)`).
PAUSED = (0xFF, 0xD0, 0x50)


def judge_log(text: str) -> list[str]:
    """Failures in the serial log."""
    failures = []
    position = 0
    for marker in REQUIRED:
        found = text.find(marker, position)
        if found < 0:
            failures.append(f"the log never shows {marker!r}")
        else:
            position = found + len(marker)
    for marker in FORBIDDEN:
        if marker in text:
            line = next(line for line in text.splitlines() if marker in line)
            failures.append(f"the log shows {line.strip()!r}")
    return failures


def judge_sound(data: bytes) -> list[str]:
    """Failures in the recording: at least [`MIN_NOTES`] of the melody's
    notes, each held for [`NOTE_MIN_MS`] within [`NOTE_TOLERANCE`]."""
    try:
        wav = analyze_wav.read_wav(data)
    except (analyze_wav.WavError, OSError) as error:
        return [f"the recording did not read: {error}"]
    segments = analyze_wav.find_segments(wav)
    heard = []
    for wanted in NOTES:
        best = None
        for segment in segments:
            if segment.freq_hz is None or segment.duration_ms < NOTE_MIN_MS:
                continue
            if abs(segment.freq_hz - wanted) <= wanted * NOTE_TOLERANCE:
                best = segment
                break
        if best is not None:
            heard.append(f"{wanted:.0f} Hz for {best.duration_ms:.0f} ms")
    if len(heard) < MIN_NOTES:
        found = ", ".join(
            f"{s.freq_hz:.0f} Hz/{s.duration_ms:.0f} ms"
            for s in segments
            if s.freq_hz is not None
        ) or "silence"
        return [
            f"the recording holds {len(heard)} of the melody's notes "
            f"(heard: {found})"
        ]
    return []


def _pixels(path: Path) -> tuple[int, int, bytes] | None:
    """The PNG's width, height and RGB rows, or `None` when it did not read."""
    try:
        width, height, channels, pixels = pngstats.decode_png(path)
    except (ValueError, OSError) as error:
        print(f"nes judge: {path}: {error}")
        return None
    return width, height, pixels, channels  # type: ignore[return-value]


def _count(pixels: bytes, channels: int, color: tuple[int, int, int],
           tolerance: int = 0) -> int:
    """How many pixels are (within `tolerance` of) `color`."""
    count = 0
    for at in range(0, len(pixels), channels):
        red, green, blue = pixels[at], pixels[at + 1], pixels[at + 2]
        if (
            abs(red - color[0]) <= tolerance
            and abs(green - color[1]) <= tolerance
            and abs(blue - color[2]) <= tolerance
        ):
            count += 1
    return count


def _box(pixels: bytes, channels: int, width: int, color: tuple[int, int, int]) -> tuple[int, int, int, int] | None:
    """The bounding box `(left, top, right, bottom)` of `color`, or `None`."""
    box = None
    for at in range(0, len(pixels), channels):
        if (pixels[at], pixels[at + 1], pixels[at + 2]) != color:
            continue
        index = at // channels
        x, y = index % width, index // width
        if box is None:
            box = [x, y, x, y]
        else:
            box[0], box[1] = min(box[0], x), min(box[1], y)
            box[2], box[3] = max(box[2], x), max(box[3], y)
    return tuple(box) if box else None  # type: ignore[return-value]


def judge_shots(shots: dict[str, Path]) -> list[str]:
    """Failures in the screenshots: the console's picture, the ball whole and
    moved, and the pause overlay. All three named shots are required."""
    failures = []
    frames: dict[str, tuple[int, int, bytes, int]] = {}
    for name in ("playing", "moved", "paused"):
        path = shots.get(name)
        if path is None or not path.exists():
            failures.append(f"{name}: {path or 'no path'} is missing")
            continue
        read = _pixels(path)
        if read is None:
            failures.append(f"{name}: {path} did not decode")
            continue
        width, height, pixels, channels = read
        frames[name] = (width, height, pixels, channels)
        backdrop = _count(pixels, channels, BACKDROP)
        text = _count(pixels, channels, TEXT)
        if backdrop < width * height // 4:
            failures.append(f"{name}: the console's backdrop is not there ({backdrop} pixels)")
        if text < 50:
            failures.append(f"{name}: the sample ROM's text is not there ({text} pixels)")
        ball = _box(pixels, channels, width, BALL)
        if ball is None:
            failures.append(f"{name}: the ball is not there")
        elif ball[3] - ball[1] + 1 < BALL_ROWS:
            failures.append(
                f"{name}: the ball is {ball[2] - ball[0] + 1}x{ball[3] - ball[1] + 1}, "
                "not 16x15: its top sprites were eaten (leave OAMADDR at 0)"
            )
    if "playing" in frames and "moved" in frames:
        _, _, before, _ = frames["playing"]
        _, _, after, _ = frames["moved"]
        first = _box(before, frames["playing"][3], frames["playing"][0], BALL)
        second = _box(after, frames["moved"][3], frames["moved"][0], BALL)
        if first is None or second is None:
            pass  # the missing ball is already reported above
        elif abs(second[0] - first[0]) < BALL_MOVE:
            failures.append(
                f"the ball did not move between the first two shots "
                f"(x {first[0]} -> {second[0]})"
            )
    if "paused" in frames:
        _, _, pixels, channels = frames["paused"]
        overlay = _count(pixels, channels, PAUSED, tolerance=24)
        if overlay < 20:
            failures.append(f"the pause overlay is not there ({overlay} pixels)")
    return failures
