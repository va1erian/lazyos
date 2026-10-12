#!/usr/bin/env python3
"""Tests for the Netboot switch of the Advanced tab (docs/netboot-plan.md N1).

Run: python tools/lazygui/test_netboot.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_config  # noqa: E402


class NetbootTests(unittest.TestCase):
    """The launcher can also write target/netboot, the stick's payload as loose files."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_checkbox_sets_the_payload_and_stick_profile_switches(self) -> None:
        env = catalog.build_env({**self.base(), "netboot": True})
        self.assertEqual(env["LAZYOS_NETBOOT"], "1")
        self.assertEqual(env["LAZYOS_USB"], "1")
        self.assertEqual(env["LAZYOS_SERVICES"], "1")
        self.assertNotIn("LAZYOS_NETBOOT_KEY", env)
        self.assertNotIn("LAZYOS_USB_IMAGE", env)

    def test_the_key_field_signs_the_manifest(self) -> None:
        env = catalog.build_env({**self.base(), "netboot": True, "netboot_key": "  target/netboot/key.sec "})
        self.assertEqual(env["LAZYOS_NETBOOT_KEY"], "target/netboot/key.sec")
        # A key without the checkbox does nothing.
        env = catalog.build_env({**self.base(), "netboot_key": "target/netboot/key.sec"})
        self.assertNotIn("LAZYOS_NETBOOT", env)
        self.assertNotIn("LAZYOS_NETBOOT_KEY", env)

    def test_off_by_default(self) -> None:
        self.assertNotIn("LAZYOS_NETBOOT", catalog.build_env({**self.base(), "desktop": True}))
        self.assertNotIn("LAZYOS_NETBOOT", catalog.build_env(demo_config(**self.base())))

    def test_with_the_stick_image_both_switches_are_set(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "netboot": True, "usb_image": True})
        for switch in ("LAZYOS_NETBOOT", "LAZYOS_USB_IMAGE", "LAZYOS_USB", "LAZYOS_DESKTOP"):
            self.assertEqual(env[switch], "1", switch)

    def test_the_image_build_carries_it(self) -> None:
        _steps, env = catalog.image_build(demo_config(**self.base(), netboot=True))
        self.assertEqual(env["LAZYOS_NETBOOT"], "1")

    def test_the_simple_tab_does_not_inherit_it(self) -> None:
        cfg = catalog.simple_config(demo_config(netboot=True), "dev", "Desktop")
        self.assertNotIn("LAZYOS_NETBOOT", catalog.build_env(cfg))


if __name__ == "__main__":
    unittest.main()
