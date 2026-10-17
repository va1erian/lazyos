#!/usr/bin/env python3
"""Tests for USB sticks plugged into the demo (`--usb-stick`, Advanced tab).

Run: python tools/lazygui/test_usb_sticks.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv  # noqa: E402


class UsbStickTests(unittest.TestCase):
    """Sticks in the guest need the driver; each path is one `--usb-stick`."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_a_stick_builds_the_usb_driver_and_a_services_session(self) -> None:
        env = catalog.build_env({**self.base(), "usb_sticks": "a.img"})
        self.assertEqual(env["LAZYOS_USB"], "1")
        self.assertEqual(env["LAZYOS_SERVICES"], "1")
        self.assertNotIn("LAZYOS_USB_IMAGE", env)

    def test_no_sticks_no_driver(self) -> None:
        self.assertNotIn("LAZYOS_USB", catalog.build_env({**self.base(), "usb_sticks": "  "}))
        self.assertNotIn("LAZYOS_USB", catalog.build_env(self.base()))

    def test_every_path_is_passed_once(self) -> None:
        argv = demo_argv(usb_sticks='a.img "my dir/b.img"')
        self.assertEqual([argv[i + 1] for i, a in enumerate(argv) if a == "--usb-stick"],
                         ["a.img", "my dir/b.img"])

    def test_none_by_default(self) -> None:
        self.assertNotIn("--usb-stick", demo_argv())


if __name__ == "__main__":
    unittest.main()
