#!/usr/bin/env python3
"""Launcher tests for the Network cards control (`run_demo --nics`, each card
on its own QEMU user network). `test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_net.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv  # noqa: E402


class NetworkCardTests(unittest.TestCase):
    def test_network_cards_are_run_demos_nics_flag(self) -> None:
        self.assertNotIn("--nics", demo_argv(net=True), "one card is the default")
        argv = demo_argv(net=True, nics="2")
        self.assertEqual(argv[argv.index("--nics") + 1], "2")
        top = str(catalog.qemu_net.MAX_NICS)
        self.assertIn("--nics", demo_argv(net=True, nics=top))
        self.assertIn("--nics", demo_argv(smb=True, nics="2"), "any networked image")

    def test_network_cards_are_bounded(self) -> None:
        for bad in ("0", str(catalog.qemu_net.MAX_NICS + 1), "-1"):
            with self.assertRaises(ValueError):
                demo_argv(net=True, nics=bad)

    def test_network_cards_are_ignored_without_networking(self) -> None:
        self.assertNotIn("--nics", demo_argv(nics="3"))
        self.assertEqual(catalog.net_flags({"nics": "99"}), [], "not even validated")

    def test_network_cards_reach_the_screenshot_and_session_modes(self) -> None:
        common = {"profile": "dev", "skip_build": True, "accel": "auto", "memory": "256M",
                  "qemu": "", "out": "shots", "net": True, "nics": "2"}
        shot = catalog.build_plan({**common, "mode": "Headless screenshots", "times": "5"})
        self.assertIn("--nics", shot[-1]["argv"])


if __name__ == "__main__":
    unittest.main()
