#!/usr/bin/env python3
"""Launcher tests for Wi-Fi (`LAZYOS_WIFI`, docs/wifi-rtw88-plan.md): the
Advanced tab switch, the Simple tab group, run_demo's `--wifi`, the firmware
fetch before a build, and the NUC stick (Wi-Fi + dbgd control + USB image).
`test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_wifi.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.simple import SIMPLE_EXTRAS  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402

FETCH = "tools/firmware/fetch.py"


def base(**overrides) -> dict:
    cfg = {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
           "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}
    cfg.update(overrides)
    return cfg


class WifiTests(unittest.TestCase):
    def test_the_switch_sets_the_build_variable_and_the_stack(self) -> None:
        for desktop in (False, True):
            env = catalog.build_env(base(desktop=desktop, wifi=True))
            self.assertEqual(env["LAZYOS_WIFI"], "1")
            self.assertEqual(env["LAZYOS_NETD"], "1", "wlan0 is one of netd's interfaces")
            self.assertNotIn("LAZYOS_WIFI", catalog.build_env(base(desktop=desktop)))

    def test_it_brings_networking_to_qemu(self) -> None:
        self.assertIn("--net", catalog.net_flags({"wifi": True}))
        self.assertEqual(catalog.net_flags({"wifi": False}), [])

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--wifi", demo_argv(wifi=True, skip_build=False))
        self.assertNotIn("--wifi", demo_argv(skip_build=False))
        self.assertNotIn("--wifi", demo_argv(wifi=True, skip_build=True), "nothing to build")

    def test_a_build_fetches_the_firmware_first(self) -> None:
        cfg = demo_config(**base(desktop=False, wifi=True, mode="Build xui app"))
        steps, env = catalog.image_build(cfg | {"profile": "dev"})
        fetch = [i for i, step in enumerate(steps) if FETCH in step["argv"]]
        self.assertEqual(len(fetch), 1)
        self.assertLess(fetch[0], len(steps) - 1, "before cargo build")
        self.assertEqual(steps[-1]["argv"][1], "build")
        self.assertEqual(env["LAZYOS_WIFI"], "1")
        no_wifi, _ = catalog.image_build(cfg | {"wifi": False})
        self.assertFalse(any(FETCH in step["argv"] for step in no_wifi))

    def test_the_simple_tab_has_a_wifi_group_that_turns_networking_on(self) -> None:
        keys = [key for key, *_ in catalog.SIMPLE_GROUPS]
        self.assertEqual(keys[keys.index("networking") + 1], "wifi", "next to Networking")
        self.assertIn("wifi", SIMPLE_EXTRAS)
        for iface in ("CLI", "Desktop"):
            cfg = catalog.simple_config(demo_config(), "dev", iface,
                                        **catalog.group_switches({"wifi"}))
            self.assertTrue(cfg["wifi"] and cfg["net"], iface)
            self.assertIn("--wifi", catalog.build_plan(cfg)[-1]["argv"])
        plain = catalog.simple_config(demo_config(), "dev", "Desktop",
                                      **catalog.group_switches({"networking"}))
        self.assertFalse(plain["wifi"], "Networking alone does not fetch firmware")

    def test_the_nuc_stick_is_wifi_dbgd_control_and_the_usb_image(self) -> None:
        env = catalog.build_env(base(desktop=True, wifi=True, dbgd_control=True,
                                     usb_image=True))
        for name in ("LAZYOS_WIFI", "LAZYOS_NETD", "LAZYOS_DBGD", "LAZYOS_DBGD_CONTROL",
                     "LAZYOS_USB_IMAGE"):
            self.assertEqual(env[name], "1", name)


if __name__ == "__main__":
    unittest.main()
