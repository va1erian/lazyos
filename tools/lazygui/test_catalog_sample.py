#!/usr/bin/env python3
"""Launcher tests for the sampling socket (`--sample-port`, issue #861): the
Advanced tab's "Sample port" reaches run_demo, also with "Skip build", and a
bad port is refused before anything runs. `test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_sample.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


class SamplePortTests(unittest.TestCase):
    def test_the_port_reaches_run_demo(self) -> None:
        for skip_build in (True, False):
            argv = demo_argv(skip_build=skip_build, sample_port="5555")
            self.assertEqual(argv[argv.index("--sample-port") + 1], "5555")

    def test_no_port_adds_nothing(self) -> None:
        self.assertNotIn("--sample-port", demo_argv())
        self.assertNotIn("--sample-port", demo_argv(sample_port="  "))

    def test_a_bad_port_is_refused(self) -> None:
        for bad in ("abc", "80", "70000", "-5", "55 55"):
            with self.assertRaises(ValueError, msg=bad):
                catalog.build_plan(demo_config(sample_port=bad))

    def test_the_other_modes_do_not_take_it(self) -> None:
        plan = catalog.build_plan(demo_config(mode="Headless screenshots", out="shots",
                                              times="2", sample_port="5555"))
        self.assertTrue(all("--sample-port" not in step["argv"] for step in plan))


if __name__ == "__main__":
    unittest.main()
