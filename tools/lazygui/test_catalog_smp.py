#!/usr/bin/env python3
"""Launcher tests for the guest vCPU count (run_demo --smp, QEMU -smp): the
Advanced tab's "CPUs" field reaches every mode's QEMU command, also with
"Skip build", and a bad count is refused before anything runs. The APs stay
parked in firmware until the kernel's SMP bring-up (docs/smp-plan.md), so N
is safe to ask for today. `test_catalog.py` runs these too.

Run: python tools/lazygui/test_catalog_smp.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


class SmpTests(unittest.TestCase):
    def test_the_count_reaches_run_demo(self) -> None:
        for skip_build in (True, False):
            argv = demo_argv(skip_build=skip_build, smp="4")
            self.assertEqual(argv[argv.index("--smp") + 1], "4")

    def test_no_count_adds_nothing(self) -> None:
        self.assertNotIn("--smp", demo_argv())
        self.assertNotIn("--smp", demo_argv(smp="  "))

    def test_one_cpu_is_explicit_not_omitted(self) -> None:
        # `--smp 1` is the promise the acceptance criteria are written against:
        # an explicit single vCPU, not QEMU's default.
        argv = demo_argv(smp="1")
        self.assertEqual(argv[argv.index("--smp") + 1], "1")

    def test_a_bad_count_is_refused(self) -> None:
        for bad in ("abc", "0", "-2", "65", "4 4"):
            with self.assertRaises(ValueError, msg=bad):
                catalog.build_plan(demo_config(smp=bad))

    def test_every_mode_takes_it(self) -> None:
        # The scripted session is driven by the same helper (its step builds
        # `smp_argv` too); the other three modes are judged here directly.
        for mode, label in (("Headless screenshots", "Capture screenshots"),
                            ("Kernel test suite", "Kernel test suite")):
            cfg = demo_config(mode=mode, out="shots", times="2", skip_build=True, smp="4")
            plan = catalog.build_plan(cfg)
            step = next(s for s in plan if s["label"] == label)
            self.assertEqual(step["argv"][step["argv"].index("--smp") + 1], "4",
                             msg=mode)


if __name__ == "__main__":
    unittest.main()
