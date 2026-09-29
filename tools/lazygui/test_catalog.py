#!/usr/bin/env python3
"""Tests for the launcher's data-volume plan flags (issue #332).

Run: python tools/lazygui/test_catalog.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402


def demo_config(**overrides) -> dict:
    """A minimal Interactive-demo configuration."""
    cfg = {"mode": "Interactive demo", "profile": "dev", "skip_build": True,
           "headless": False, "accel": "auto", "memory": "256M", "qemu": "", "extra": ""}
    cfg.update(overrides)
    return cfg


def demo_argv(**overrides) -> list[str]:
    return catalog.build_plan(demo_config(**overrides))[-1]["argv"]


class DataDiskPlanTests(unittest.TestCase):
    def test_attached_by_default_at_the_standard_path(self) -> None:
        argv = demo_argv()
        self.assertEqual(argv[argv.index("--data-disk") + 1], catalog.DATA_IMAGE)
        self.assertNotIn("--no-data-disk", argv)

    def test_custom_path_is_passed_through(self) -> None:
        argv = demo_argv(data_path="D:/vols/x.img")
        self.assertEqual(argv[argv.index("--data-disk") + 1], "D:/vols/x.img")

    def test_toggle_off_detaches(self) -> None:
        argv = demo_argv(data_disk=False)
        self.assertIn("--no-data-disk", argv)
        self.assertNotIn("--data-disk", argv)


if __name__ == "__main__":
    unittest.main()
