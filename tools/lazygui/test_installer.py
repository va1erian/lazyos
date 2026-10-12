#!/usr/bin/env python3
"""Tests for the stick's `lazyos-install` switch (docs/grub-install-plan.md I2).

Run: python tools/lazygui/test_installer.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


class InstallerTests(unittest.TestCase):
    """The Advanced tab's installer checkbox puts lazyos-install on the stick."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_writes_the_stick_with_the_installer(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "installer": True})
        for switch in ("LAZYOS_INSTALLER", "LAZYOS_USB_IMAGE", "LAZYOS_USB"):
            self.assertEqual(env[switch], "1", switch)

    def test_off_by_default(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "usb_image": True})
        self.assertNotIn("LAZYOS_INSTALLER", env)

    def test_run_demo_builds_it(self) -> None:
        self.assertIn("--installer", demo_argv(installer=True, skip_build=False))
        self.assertNotIn("--installer", demo_argv(installer=True, skip_build=True))

    def test_build_steps_build_it_before_cargo(self) -> None:
        cfg = demo_config(installer=True, mode="Headless screenshots", skip_build=False,
                          out="shots", times="2", desktop=True)
        labels = [step["label"] for step in catalog.app_steps(cfg)]
        self.assertIn("Build lazyos-install (static musl)", labels)
        self.assertEqual(catalog.app_steps(demo_config()), [])


if __name__ == "__main__":
    unittest.main()
