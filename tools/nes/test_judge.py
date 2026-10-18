#!/usr/bin/env python3
"""Self-test of tools/nes/judge.py: the judge passes a good run and fails the
ways a broken one fails (a missing marker, a refusal in the log, silence or a
wrong note in the recording, a ball that never moved, a missing overlay)."""

from __future__ import annotations

import io
import struct
import sys
import unittest
import wave
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import judge  # noqa: E402


def png(width: int, height: int, pixels: list[tuple[int, int, int]]) -> bytes:
    """A truecolour PNG from a row-major list of RGB tuples, in the standard
    library: the fixtures need no Pillow (CI has none)."""

    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    rows = bytearray()
    for y in range(height):
        rows.append(0)  # filter: none
        for pixel in pixels[y * width:(y + 1) * width]:
            rows.extend(pixel)
    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(bytes(rows), 6)) + chunk(b"IEND", b""))

GOOD_LOG = """\
TERM:UP:PASS
NES:SOUND:PASS:48000:1
NES:LOAD:PASS:/system/share/samples/lazyos-sample.nes:mapper=0:prg=32K:chr=8K:battery=1:region=ntsc
NES:RUN:PASS:frames=120
NES:STATE:PASS:save=1:/home/user/.apps/os.lazy.nes/states/sample/slot-1.state
NES:STATE:PASS:load=1:/home/user/.apps/os.lazy.nes/states/sample/slot-1.state
NES:QUIT:PASS
NES:HEADLESS:PASS:frames=300 crc=1a2b3c4d
"""


def tone(hz: float, ms: int, rate: int = 48_000) -> bytes:
    """A square wave at `hz`, `ms` long, as 16-bit mono samples."""
    period = max(1, int(rate / hz))
    samples = bytearray()
    for at in range(rate * ms // 1000):
        value = 12_000 if (at // (period // 2)) % 2 == 0 else -12_000
        samples += int(value).to_bytes(2, "little", signed=True)
    return bytes(samples)


def wav_bytes(parts: list[tuple[float | None, int]], rate: int = 48_000) -> bytes:
    """A WAV of `(frequency or silence, ms)` parts."""
    out = io.BytesIO()
    with wave.open(out, "wb") as handle:
        handle.setnchannels(1)
        handle.setsampwidth(2)
        handle.setframerate(rate)
        for hz, ms in parts:
            handle.writeframes(
                bytes(rate * ms // 1000 * 2) if hz is None else tone(hz, ms, rate)
            )
    return out.getvalue()


def melody() -> bytes:
    """The sample ROM's four notes with a breath between them."""
    parts: list[tuple[float | None, int]] = []
    for hz in judge.NOTES:
        parts.append((hz, 500))
        parts.append((None, 120))
    return wav_bytes(parts)


class Log(unittest.TestCase):
    def test_a_good_log_passes(self):
        self.assertEqual(judge.judge_log(GOOD_LOG), [])

    def test_a_missing_marker_fails(self):
        broken = GOOD_LOG.replace("NES:RUN:PASS:frames=120\n", "")
        failures = judge.judge_log(broken)
        self.assertTrue(any("NES:RUN:PASS:frames=" in f for f in failures), failures)

    def test_out_of_order_markers_fail(self):
        broken = GOOD_LOG.replace("NES:RUN:PASS:frames=120\n", "").replace(
            "NES:QUIT:PASS", "NES:RUN:PASS:frames=120\nNES:QUIT:PASS"
        )
        self.assertTrue(judge.judge_log(broken))

    def test_a_refusal_fails(self):
        broken = GOOD_LOG + "NES:FAIL:no such ROM: /roms/gone.nes\n"
        failures = judge.judge_log(broken)
        self.assertTrue(any("no such ROM" in f for f in failures), failures)


class Sound(unittest.TestCase):
    def test_the_melody_passes(self):
        self.assertEqual(judge.judge_sound(melody()), [])

    def test_silence_fails(self):
        failures = judge.judge_sound(wav_bytes([(None, 1500)]))
        self.assertTrue(any("of the melody's notes" in f for f in failures), failures)

    def test_one_note_fails(self):
        failures = judge.judge_sound(wav_bytes([(440.0, 500), (None, 200)]))
        self.assertTrue(any("of the melody's notes" in f for f in failures), failures)

    def test_a_wrong_pitch_fails(self):
        # A C#5 where C5 belongs: outside the tolerance.
        parts = [(440.0, 500), (None, 120), (554.365, 500), (None, 120)]
        failures = judge.judge_sound(wav_bytes(parts))
        self.assertTrue(any("of the melody's notes" in f for f in failures), failures)

    def test_a_file_that_is_not_a_wav_fails(self):
        failures = judge.judge_sound(b"not a wav at all")
        self.assertTrue(any("did not read" in f for f in failures), failures)


class Shots(unittest.TestCase):
    """The screenshot judge, on PNGs written by [`png`]."""

    SIZE = (320, 240)

    def setUp(self):
        self.dir = Path(__file__).resolve().parent / "target-test"
        self.dir.mkdir(exist_ok=True)
        self.shots = {}
        for name, ball_x in [("playing", 120), ("moved", 180), ("paused", 120)]:
            self.write(name, self.picture(ball_x, paused=name == "paused"))

    def picture(self, ball_x: int, paused: bool = False,
                clip_ball: int = 0) -> list[tuple[int, int, int]]:
        """A console-like frame: the backdrop, text, the ball (16x15 at
        `ball_x`), and the pause overlay. `clip_ball` drops that many of the
        ball's top rows (the OAMADDR quirk)."""
        width, height = self.SIZE
        pixels = [judge.BACKDROP] * (width * height)
        for x in range(20, 120, 4):
            for y in range(20, 30):
                pixels[y * width + x] = judge.TEXT
        for x in range(ball_x, ball_x + 16):
            for y in range(100 + clip_ball, 115):
                pixels[y * width + x] = judge.BALL
        if paused:
            for x in range(200, 230):
                for y in range(180, 210):
                    pixels[y * width + x] = judge.PAUSED
        return pixels

    def write(self, name: str, pixels: list[tuple[int, int, int]]) -> None:
        path = self.dir / f"{name}.png"
        path.write_bytes(png(*self.SIZE, pixels))
        self.shots[name] = path

    def tearDown(self):
        for path in self.dir.glob("*.png"):
            path.unlink()
        self.dir.rmdir()

    def test_a_good_run_passes(self):
        self.assertEqual(judge.judge_shots(self.shots), [])

    def test_a_still_ball_fails(self):
        # `moved` with the ball where `playing` has it: the D-pad never
        # reached the game.
        self.write("moved", self.picture(120))
        failures = judge.judge_shots(self.shots)
        self.assertTrue(any("did not move" in f for f in failures), failures)

    def test_a_half_drawn_ball_fails(self):
        # The OAMADDR quirk: only the ball's bottom half is drawn.
        self.write("playing", self.picture(120, clip_ball=8))
        failures = judge.judge_shots(self.shots)
        self.assertTrue(any("16x15" in f for f in failures), failures)

    def test_a_missing_overlay_fails(self):
        self.write("paused", self.picture(120))
        failures = judge.judge_shots(self.shots)
        self.assertTrue(any("pause overlay" in f for f in failures), failures)

    def test_a_missing_shot_fails(self):
        del self.shots["paused"]
        failures = judge.judge_shots(self.shots)
        self.assertTrue(any("is missing" in f for f in failures), failures)


if __name__ == "__main__":
    unittest.main(verbosity=2)
