#!/usr/bin/env python3
"""Tests for the USB stick image switch, Advanced and Simple (docs/usb-stick.md).

Run: python tools/lazygui/test_usb_image.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_config  # noqa: E402


class UsbImageTests(unittest.TestCase):
    """The launcher can also write the USB stick image (docs/usb-stick.md)."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_build_variable(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "usb_image": True})
        self.assertEqual(env["LAZYOS_USB_IMAGE"], "1")
        self.assertEqual(env["LAZYOS_USB"], "1")
        self.assertEqual(env["LAZYOS_SERVICES"], "1")

    def test_off_by_default(self) -> None:
        self.assertNotIn("LAZYOS_USB_IMAGE", catalog.build_env({**self.base(), "desktop": True}))

    def test_simple_tab_choice_is_desktop_only(self) -> None:
        on = catalog.simple_config(demo_config(), "dev", "Desktop", usb_image=True)
        self.assertTrue(on["usb_image"])
        self.assertEqual(catalog.build_env(on)["LAZYOS_USB_IMAGE"], "1")
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "CLI", usb_image=True)["usb_image"])

    def test_simple_does_not_inherit_the_advanced_checkbox(self) -> None:
        cfg = catalog.simple_config(demo_config(usb_image=True), "dev", "Desktop")
        self.assertFalse(cfg["usb_image"])


if __name__ == "__main__":
    unittest.main()
