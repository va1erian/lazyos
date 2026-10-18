#!/usr/bin/env python3
"""Self-test of tools/nes/judge.py: the judge passes a good run and fails the
ways a broken one fails (a missing marker, a refusal in the log, silence or a
wrong note in the recording, a ball that never moved, a missing overlay)."""

from __future__ import annotations

import io
import sys
import unittest
import wave
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

import judge  # noqa: E402

GOOD_LOG = """\
TERM:UP:PASS
NES:SOUND:PASS:48000:1
NES:LOAD:PASS:/system/share/samples/lazyos-sample.nes:mapper=0:prg=32K:chr=8K:battery=1:region=ntsc
NES:RUN:PASS:frames=120
NES:STATE:PASS:save=1:/home/user/.apps/os.lazy.nes/states/sample/slot-1.state
NES:STATE:PASS:load=1:/home/user/.apps/os.lazy.nes/states/sample/slot-1.state
NES:QUIT:PASS
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
    def setUp(self):
        from PIL import Image

        self.dir = Path(__file__).resolve().parent / "target-test"
        self.dir.mkdir(exist_ok=True)
        self.shots = {}
        for name, extra in [
            ("playing", None),
            ("moved", (237, 152, 31)),
            ("paused", judge.PAUSED),
        ]:
            image = Image.new("RGB", (320, 240), judge.BACKDROP)
            # Text-like pixels, and a block that differs between the shots.
            for x in range(20, 120, 4):
                for y in range(20, 30):
                    image.putpixel((x, y), judge.TEXT)
            if extra is not None:
                for x in range(200, 230):
                    for y in range(180, 210):
                        image.putpixel((x, y), extra)
            image.save(self.dir / f"{name}.png")
            self.shots[name] = self.dir / f"{name}.png"

    def tearDown(self):
        for path in self.dir.glob("*.png"):
            path.unlink()
        self.dir.rmdir()

    def test_a_good_run_passes(self):
        self.assertEqual(judge.judge_shots(self.shots), [])

    def test_a_still_ball_fails(self):
        # `moved` identical to `playing`: the D-pad never reached the game.
        import shutil

        shutil.copyfile(self.shots["playing"], self.shots["moved"])
        failures = judge.judge_shots(self.shots)
        self.assertTrue(any("did not move" in f for f in failures), failures)

    def test_a_missing_overlay_fails(self):
        import shutil

        shutil.copyfile(self.shots["playing"], self.shots["paused"])
        failures = judge.judge_shots(self.shots)
        self.assertTrue(any("pause overlay" in f for f in failures), failures)

    def test_a_missing_shot_fails(self):
        del self.shots["paused"]
        failures = judge.judge_shots(self.shots)
        self.assertTrue(any("is missing" in f for f in failures), failures)


if __name__ == "__main__":
    unittest.main(verbosity=2)
