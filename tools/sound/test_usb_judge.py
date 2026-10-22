#!/usr/bin/env python3
"""The USB audio harness's judge fails when it should (`usb_run.judge`).

Run: python tools/sound/test_usb_judge.py
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "screenshot"))
import usb_run  # noqa: E402

GOOD = "\n".join(usb_run.PASS_MARKERS)


class JudgeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.dir = tempfile.TemporaryDirectory()
        self.wav = Path(self.dir.name) / "usb.wav"

    def tearDown(self) -> None:
        self.dir.cleanup()

    def test_a_missing_marker_fails(self) -> None:
        text = GOOD.replace("USBD:AUDIO:UP", "")
        self.assertEqual(usb_run.judge(text, self.wav, None), 1)

    def test_a_fail_marker_fails_even_with_every_pass_marker(self) -> None:
        self.assertEqual(usb_run.judge(GOOD + "\nUSBD:AUDIO:FAIL port=0-1 x", self.wav, None), 1)
        self.assertEqual(usb_run.judge(GOOD + "\nBEEP:SOAK:FAIL iteration 3", self.wav, None), 1)

    def test_no_recording_fails(self) -> None:
        self.assertEqual(usb_run.judge(GOOD, self.wav, None), 1)

    def test_two_outputs_must_both_be_listed(self) -> None:
        virtio = Path(self.dir.name) / "virtio.wav"
        text = GOOD + "\n" + "\n".join(usb_run.BOTH_MARKERS) + "\nAUDIOD:OUTPUTS usb-audio0"
        self.assertEqual(usb_run.judge(text, self.wav, virtio), 1)


if __name__ == "__main__":
    unittest.main()
