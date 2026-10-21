#!/usr/bin/env python3
"""Tests for run_demo.py's `--gpud-audio` (no QEMU, no build).

Run: python tools/test_run_demo_gpud.py
"""

from __future__ import annotations

import os
import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
# A module import, so unittest does not collect MainTests a second time here.
import test_run_demo  # noqa: E402


class GpudAudioTests(test_run_demo.RunDemoCase):
    def test_gpud_audio_implies_gpud_and_passes_its_arguments(self) -> None:
        with mock.patch.dict(os.environ):
            os.environ["LAZYOS_GPUD_ARGS"] = "audio=1"  # inherited: never wins
            self.run_main("--intelgfx")
            self.assertEqual(self.builds[-1].get("LAZYOS_GPUD_ARGS"), "")
            code, _ = self.run_main("--gpud-audio")
            self.assertEqual(code, 0)
            build = self.builds[-1]
            self.assertEqual(build.get("LAZYOS_INTELGFX"), "1")
            self.assertEqual(build.get("LAZYOS_GPUD_ARGS"), "audio=1 edid=/system/share/gpu/edid.bin")
            self.run_main("--gpud-audio", "--gpud-edid", "/system/share/mon.bin")
            self.assertEqual(self.builds[-1].get("LAZYOS_GPUD_ARGS"), "audio=1 edid=/system/share/mon.bin")


if __name__ == "__main__":
    unittest.main()
