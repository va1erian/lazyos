#!/usr/bin/env python3
"""Launcher tests for the display mode (HiDPI, docs/hidpi-plan.md): the
Simple tab's HiDPI choice, the Advanced tab's mode field and run_demo's
`--hidpi`/`--display-mode`. `test_catalog.py` runs them too.

Run: python tools/lazygui/test_display.py
"""

from __future__ import annotations

import argparse
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog, display  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


def parse(argv: list[str]) -> argparse.Namespace:
    """run_demo's display options (plus `--no-build`) over ``argv``."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--no-build", action="store_true")
    display.add_display_options(parser)
    return parser.parse_args(argv)


class DisplayModeTests(unittest.TestCase):
    def test_modes_are_checked_like_the_image_build(self) -> None:
        self.assertEqual(display.check_mode(" 2560X1440 "), "2560x1440")
        self.assertEqual(display.check_mode(""), "")
        for bad in ("2560", "2560x", "4000x2000", "639x480", "2560x1440x2", "-1x5", "axb"):
            with self.assertRaises(ValueError, msg=bad):
                display.check_mode(bad)

    def test_simple_hidpi_sets_the_mode(self) -> None:
        for iface in ("Desktop", "CLI"):
            cfg = catalog.simple_config(demo_config(), "dev", iface, hidpi=True)
            self.assertEqual(catalog.build_env(cfg)["LAZYOS_DISPLAY_MODE"], "2560x1440")
            argv = catalog.build_plan(cfg)[-1]["argv"]
            self.assertIn("--display-mode", argv)
            self.assertEqual(argv[argv.index("--display-mode") + 1], "2560x1440")

    def test_off_by_default(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop")
        self.assertNotIn("LAZYOS_DISPLAY_MODE", catalog.build_env(cfg))
        self.assertNotIn("--display-mode", catalog.build_plan(cfg)[-1]["argv"])
        self.assertNotIn("--display-mode", demo_argv())

    def test_advanced_field_is_honoured_and_needs_a_build(self) -> None:
        argv = demo_argv(skip_build=False, display_mode="1920x1080")
        self.assertEqual(argv[argv.index("--display-mode") + 1], "1920x1080")
        with self.assertRaises(ValueError):
            catalog.build_plan(demo_config(skip_build=True, display_mode="2560x1440"))
        with self.assertRaises(ValueError):
            catalog.build_plan(demo_config(skip_build=False, display_mode="huge"))

    def test_run_demo_flags(self) -> None:
        self.assertEqual(display.build_display(parse(["--hidpi"])),
                         {"LAZYOS_DISPLAY_MODE": "2560x1440"})
        self.assertEqual(display.build_display(parse(["--display-mode", "1920x1080"])),
                         {"LAZYOS_DISPLAY_MODE": "1920x1080"})
        self.assertEqual(display.build_display(parse([])), {})
        for argv in (["--hidpi", "--no-build"],
                     ["--hidpi", "--display-mode", "1920x1080"],
                     ["--display-mode", "10x10"]):
            with self.assertRaises(ValueError, msg=argv):
                display.build_display(parse(argv))

    def test_display_max_for_real_pcs(self) -> None:
        self.assertEqual(display.build_display(parse(["--display-max", "2560x1440"])),
                         {"LAZYOS_DISPLAY_MAX": "2560x1440"})
        argv = demo_argv(skip_build=False, display_max="2560x1440")
        self.assertEqual(argv[argv.index("--display-max") + 1], "2560x1440")
        self.assertNotIn("--display-max", demo_argv())
        with self.assertRaises(ValueError):
            catalog.build_plan(demo_config(skip_build=True, display_max="2560x1440"))
        with self.assertRaises(ValueError):
            display.build_display(parse(["--display-max", "2560x1440", "--no-build"]))


if __name__ == "__main__":
    unittest.main()
