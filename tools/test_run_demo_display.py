#!/usr/bin/env python3
"""Tests for run_demo.py's display flags: `--hidpi` (a 1280x720 desktop at 2x),
`--hires` (a 2560x1440 desktop at 1x) and `--display-scale` (no QEMU, no build).

Run: python tools/test_run_demo_display.py
"""

from __future__ import annotations

import io
import sys
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run_demo  # noqa: E402
# A module import, so unittest does not collect MainTests a second time here.
import test_run_demo  # noqa: E402


class DisplayTests(test_run_demo.RunDemoCase):
    """The faked build and QEMU of `RunDemoCase`."""

    def test_hires_builds_the_1440p_screen_at_scale_one(self) -> None:
        code, _ = self.run_main("--desktop", "--hires", "--yes")
        self.assertEqual(code, 0)
        env = self.builds[-1]
        self.assertEqual(env["LAZYOS_DISPLAY_MODE"], "2560x1440")
        self.assertEqual(env["LAZYOS_DISPLAY_SCALE"], "1")
        self.assertEqual(env["LAZYOS_DISPLAY_MAX"], "2560x1440")

    def test_hidpi_keeps_the_automatic_scale(self) -> None:
        code, _ = self.run_main("--desktop", "--hidpi", "--yes")
        self.assertEqual(code, 0)
        self.assertEqual(self.builds[-1]["LAZYOS_DISPLAY_MODE"], "2560x1440")
        self.assertNotIn("LAZYOS_DISPLAY_SCALE", self.builds[-1])

    def test_display_scale_alone_sets_only_the_scale(self) -> None:
        code, _ = self.run_main("--desktop", "--display-scale", "1", "--yes")
        self.assertEqual(code, 0)
        self.assertEqual(self.builds[-1]["LAZYOS_DISPLAY_SCALE"], "1")
        self.assertNotIn("LAZYOS_DISPLAY_MODE", self.builds[-1])

    def test_hidpi_with_hires_is_a_friendly_error(self) -> None:
        err = io.StringIO()
        with self.assertRaises(SystemExit) as raised, redirect_stderr(err), \
                redirect_stdout(io.StringIO()):
            run_demo.main(["--hidpi", "--hires", "--image", str(self.image)])
        self.assertEqual(raised.exception.code, 2)
        self.assertIn("exclude each other", err.getvalue())

    def test_a_skipped_build_cannot_take_the_flags(self) -> None:
        for flag in (["--hires"], ["--display-scale", "1"]):
            err = io.StringIO()
            with self.assertRaises(SystemExit), redirect_stderr(err), \
                    redirect_stdout(io.StringIO()):
                run_demo.main([*flag, "--no-build", "--image", str(self.image)])
            self.assertIn("need a build", err.getvalue())
        self.assertEqual(self.builds, [])


if __name__ == "__main__":
    unittest.main()
