#!/usr/bin/env python3
"""Launcher tests for the display mode and scale (HiDPI and hi-res,
docs/hidpi-plan.md): the Simple tab's HiDPI and Hi-res choices, the Advanced
tab's mode and scale fields and run_demo's `--hidpi`/`--hires`/`--display-mode`/
`--display-scale`. `test_catalog.py` runs them too.

Run: python tools/lazygui/test_display.py
"""

from __future__ import annotations

import argparse
import contextlib
import io
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
            env = catalog.build_env(cfg)
            self.assertEqual(env["LAZYOS_DISPLAY_MODE"], "2560x1440")
            # A real PC keeps its firmware framebuffer: HiDPI lifts the cap too.
            self.assertEqual(env["LAZYOS_DISPLAY_MAX"], "2560x1440")
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
                         {"LAZYOS_DISPLAY_MODE": "2560x1440", "LAZYOS_DISPLAY_MAX": "2560x1440"})
        self.assertEqual(display.build_display(parse(["--display-mode", "2560x1440"])),
                         {"LAZYOS_DISPLAY_MODE": "2560x1440", "LAZYOS_DISPLAY_MAX": "2560x1440"})
        self.assertEqual(display.build_display(parse(["--hidpi", "--display-max", "3840x2160"])),
                         {"LAZYOS_DISPLAY_MODE": "2560x1440", "LAZYOS_DISPLAY_MAX": "3840x2160"},
                         "a typed cap wins")
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


class FakeVar:
    """The slice of `tk.BooleanVar` the exclusive checkboxes use."""

    def __init__(self) -> None:
        self.value = False
        self.traces: list = []

    def get(self) -> bool:
        return self.value

    def set(self, value: bool) -> None:
        self.value = value
        for trace in list(self.traces):
            trace()

    def trace_add(self, _mode: str, callback) -> None:
        self.traces.append(callback)


class HiresTests(unittest.TestCase):
    """Hi-res: the 1440p screen at scale 1 (`LAZYOS_DISPLAY_SCALE=1`)."""

    def test_scales_are_checked_like_the_image_build(self) -> None:
        self.assertEqual(display.check_scale(" Auto "), "auto")
        self.assertEqual(display.check_scale(""), "")
        for good in ("1", "2"):
            self.assertEqual(display.check_scale(good), good)
        for bad in ("0", "3", "1.5", "two", "-1"):
            with self.assertRaises(ValueError, msg=bad):
                display.check_scale(bad)

    def test_simple_hires_sets_the_mode_and_scale_one(self) -> None:
        for iface in ("Desktop", "CLI"):
            cfg = catalog.simple_config(demo_config(), "dev", iface, hires=True)
            env = catalog.build_env(cfg)
            self.assertEqual(env["LAZYOS_DISPLAY_MODE"], "2560x1440")
            self.assertEqual(env["LAZYOS_DISPLAY_SCALE"], "1")
            self.assertEqual(env["LAZYOS_DISPLAY_MAX"], "2560x1440", "a real PC's cap is lifted too")
            argv = catalog.build_plan(cfg)[-1]["argv"]
            self.assertEqual(argv[argv.index("--display-mode") + 1], "2560x1440")
            self.assertEqual(argv[argv.index("--display-scale") + 1], "1")

    def test_hidpi_and_the_default_leave_the_scale_to_auto(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop", hidpi=True)
        self.assertNotIn("LAZYOS_DISPLAY_SCALE", catalog.build_env(cfg))
        self.assertNotIn("--display-scale", catalog.build_plan(cfg)[-1]["argv"])
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop")
        self.assertNotIn("LAZYOS_DISPLAY_SCALE", catalog.build_env(cfg))
        self.assertNotIn("--display-scale", demo_argv())

    def test_simple_choices_exclude_each_other_and_ignore_the_advanced_field(self) -> None:
        with self.assertRaises(ValueError):
            catalog.simple_config(demo_config(), "dev", "Desktop", hidpi=True, hires=True)
        base = demo_config(display_mode="1920x1080", display_scale="1")
        cfg = catalog.simple_config(base, "dev", "Desktop", hidpi=True)
        self.assertEqual(catalog.build_env(cfg)["LAZYOS_DISPLAY_MODE"], "2560x1440")
        self.assertNotIn("LAZYOS_DISPLAY_SCALE", catalog.build_env(cfg), "no Advanced leak")

    def test_the_checkboxes_clear_each_other(self) -> None:
        v = {"simple_hidpi": FakeVar(), "simple_hires": FakeVar()}
        display.link_exclusive(v)
        v["simple_hidpi"].set(True)
        self.assertEqual((v["simple_hidpi"].get(), v["simple_hires"].get()), (True, False))
        v["simple_hires"].set(True)
        self.assertEqual((v["simple_hidpi"].get(), v["simple_hires"].get()), (False, True))
        v["simple_hidpi"].set(True)
        self.assertEqual((v["simple_hidpi"].get(), v["simple_hires"].get()), (True, False))
        v["simple_hidpi"].set(False)
        self.assertEqual((v["simple_hidpi"].get(), v["simple_hires"].get()), (False, False))

    def test_advanced_scale_field_is_honoured_and_needs_a_build(self) -> None:
        argv = demo_argv(skip_build=False, display_mode="3840x2160", display_scale="1")
        self.assertEqual(argv[argv.index("--display-scale") + 1], "1")
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop") | {"display_scale": "auto"}
        env = catalog.build_env(cfg)
        self.assertEqual(env["LAZYOS_DISPLAY_SCALE"], "auto")
        with self.assertRaises(ValueError):
            catalog.build_plan(demo_config(skip_build=True, display_scale="1"))
        with self.assertRaises(ValueError):
            catalog.build_plan(demo_config(skip_build=False, display_scale="3"))

    def test_run_demo_flags(self) -> None:
        hires = {"LAZYOS_DISPLAY_MODE": "2560x1440", "LAZYOS_DISPLAY_MAX": "2560x1440",
                 "LAZYOS_DISPLAY_SCALE": "1"}
        self.assertEqual(display.build_display(parse(["--hires"])), hires)
        self.assertEqual(display.build_display(parse(["--hires", "--display-scale", "1"])), hires)
        self.assertEqual(display.build_display(parse(["--display-mode", "2560x1440",
                                                      "--display-scale", "1"])), hires)
        self.assertEqual(display.build_display(parse(["--hidpi", "--display-scale", "2"])),
                         {"LAZYOS_DISPLAY_MODE": "2560x1440", "LAZYOS_DISPLAY_MAX": "2560x1440",
                          "LAZYOS_DISPLAY_SCALE": "2"})
        self.assertEqual(display.build_display(parse(["--display-scale", "auto"])),
                         {"LAZYOS_DISPLAY_SCALE": "auto"})
        self.assertEqual(display.build_display(parse(["--hires", "--display-max", "3840x2160"])),
                         {**hires, "LAZYOS_DISPLAY_MAX": "3840x2160"})

    def test_contradictions_and_a_skipped_build_are_refused_kindly(self) -> None:
        with self.assertRaisesRegex(ValueError, "exclude each other"):
            display.build_display(parse(["--hidpi", "--hires"]))
        for argv in (["--hires", "--display-scale", "2"],
                     ["--hires", "--display-scale", "auto"],
                     ["--hidpi", "--display-scale", "1"],
                     ["--hires", "--display-mode", "1920x1080"],
                     ["--hires", "--no-build"],
                     ["--display-scale", "1", "--no-build"]):
            with self.assertRaises(ValueError, msg=argv):
                display.build_display(parse(argv))
        with self.assertRaises(SystemExit), contextlib.redirect_stderr(io.StringIO()):
            parse(["--display-scale", "3"])


if __name__ == "__main__":
    unittest.main()
