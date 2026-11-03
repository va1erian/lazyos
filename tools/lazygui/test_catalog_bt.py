#!/usr/bin/env python3
"""Launcher tests for Bluetooth (docs/bluetooth-plan.md phase B2): the Advanced
tab switch, the USB driver it brings and run_demo's `--bt`.
`test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_bt.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


class BluetoothTests(unittest.TestCase):
    """`btd` and the USB Bluetooth transport: one switch, the USB driver with it."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_build_variables_and_the_usb_driver(self) -> None:
        for desktop in (False, True):
            env = catalog.build_env({**self.base(), "desktop": desktop, "bt": True})
            self.assertEqual(env["LAZYOS_BT"], "1")
            self.assertEqual(env["LAZYOS_USB"], "1", "the controller is a USB device `usbd` drives")
            self.assertEqual(env["LAZYOS_SERVICES"], "1", "a session that starts `usbd` and `btd`")
            plain = catalog.build_env({**self.base(), "desktop": desktop})
            self.assertNotIn("LAZYOS_BT", plain)

    def test_a_chosen_services_session_is_kept(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": False, "services": True, "bt": True})
        self.assertEqual((env["LAZYOS_BT"], env["LAZYOS_SERVICES"]), ("1", "1"))

    def test_the_demo_passes_the_flag_even_without_a_build(self) -> None:
        for skip_build in (False, True):
            self.assertIn("--bt", demo_argv(bt=True, skip_build=skip_build))
        self.assertNotIn("--bt", demo_argv(skip_build=False))

    def test_the_simple_tab_never_turns_it_on(self) -> None:
        # The Advanced tab's switch never leaks into a Simple build.
        for iface in ("CLI", "Desktop"):
            leaked = catalog.simple_config(demo_config() | {"bt": True}, "dev", iface)
            self.assertFalse(leaked["bt"], iface)
            self.assertNotIn("--bt", catalog.build_plan(leaked)[-1]["argv"])


if __name__ == "__main__":
    unittest.main()
