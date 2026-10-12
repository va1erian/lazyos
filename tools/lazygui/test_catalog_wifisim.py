#!/usr/bin/env python3
"""Launcher tests for the simulated Wi-Fi radio (docs/wifi-prerequisites-plan.md
section 3.6): the Advanced tab switch, the stack it brings and run_demo's
`--wifi-sim`. `test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_wifisim.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv  # noqa: E402


class WifiSimTests(unittest.TestCase):
    """`wifisim` and its fake access points: one switch, the stack with it."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_build_variable_and_the_stack(self) -> None:
        for desktop in (False, True):
            env = catalog.build_env({**self.base(), "desktop": desktop, "wifi_sim": True})
            self.assertEqual(env["LAZYOS_WIFI_SIM"], "1")
            self.assertEqual(env["LAZYOS_NETD"], "1", "the radio is a card of the stack")
            self.assertEqual(env["LAZYOS_NETD_ARGS"], "demo=0")
            plain = catalog.build_env({**self.base(), "desktop": desktop})
            self.assertNotIn("LAZYOS_WIFI_SIM", plain)

    def test_it_adds_no_forwards_of_its_own(self) -> None:
        # Only dbgd's card and port (always on) are there, nothing for the radio.
        self.assertEqual(catalog.net_flags({"net": False, "wifi_sim": True}),
                         catalog.net_flags({"net": False, "wifi_sim": False}))

    def test_the_demo_passes_the_flag_even_without_a_build(self) -> None:
        for skip_build in (False, True):
            self.assertIn("--wifi-sim", demo_argv(wifi_sim=True, skip_build=skip_build))
        self.assertNotIn("--wifi-sim", demo_argv(skip_build=False))

    def test_the_station_manager_and_the_simulator_go_together(self) -> None:
        # The Network app's Wi-Fi tab needs `wlanmd` (--wifi) and the radio
        # (--wifi-sim); run_demo builds the network apps for the pair.
        argv = demo_argv(wifi=True, wifi_sim=True, skip_build=False)
        self.assertIn("--wifi", argv)
        self.assertIn("--wifi-sim", argv)


if __name__ == "__main__":
    unittest.main()
