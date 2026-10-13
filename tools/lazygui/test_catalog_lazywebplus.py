#!/usr/bin/env python3
"""Launcher tests for LazyWeb+ (`LAZYOS_LAZYWEBPLUS=1`, docs/lazywebplus.md): the WebKit engine in
LazyWeb's window, from the Simple and Advanced tabs and run_demo. `test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_lazywebplus.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


class LazyWebPlusTests(unittest.TestCase):
    """A desktop app that brings the desktop profile and the network stack, built in Docker (the
    bundle, the port's processes, the app) and packaged by the xui build."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_brings_the_desktop_and_the_stack(self) -> None:
        env = catalog.build_env({**self.base(), "lazywebplus": True})
        for name in ("LAZYOS_LAZYWEBPLUS", "LAZYOS_DESKTOP", "LAZYOS_NETD"):
            self.assertEqual(env.get(name), "1", name)
        self.assertNotIn("LAZYOS_WEBKIT", env)
        self.assertNotIn("LAZYOS_LAZYWEBPLUS", catalog.build_env({**self.base(), "desktop": True}))

    def test_simple_desktop_offers_it_and_cli_does_not(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop", lazywebplus=True)
        self.assertTrue(cfg["lazywebplus"] and cfg["net"])
        cli = catalog.simple_config(demo_config(), "dev", "CLI", lazywebplus=True)
        self.assertFalse(cli["lazywebplus"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "Desktop")["lazywebplus"])

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--lazywebplus", demo_argv(lazywebplus=True, skip_build=False))
        self.assertNotIn("--lazywebplus", demo_argv(skip_build=False))
        self.assertNotIn("--lazywebplus", demo_argv(lazywebplus=True, skip_build=True))
        self.assertIn("--net", demo_argv(lazywebplus=True, skip_build=False))

    def test_the_ad_blockers_updates_bring_the_https_tools(self) -> None:
        """The blocker fetches its lists with `curl` (a child under its own label, issue #845)."""
        env = catalog.build_env({**self.base(), "lazywebplus": True})
        self.assertEqual(env.get("LAZYOS_TLS"), "1")
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop", lazywebplus=True)
        self.assertTrue(cfg["tls"])
        self.assertIn("--tls", demo_argv(lazywebplus=True, tls=True, skip_build=False))
        steps = catalog.app_steps({"lazywebplus": True, "tls": True})
        self.assertIn("tools/nettls/build.py", [s["argv"][1] for s in steps])

    def test_it_builds_the_bundle_the_port_the_plus_app_then_packages_it(self) -> None:
        steps = catalog.app_steps({"lazywebplus": True})
        self.assertEqual([s["argv"][1] for s in steps],
                         ["tools/webkit/deps.py", "tools/webkit/port.py", "tools/webkit/app.py", "tools/xui/build.py"])
        self.assertIn("--plus", steps[2]["argv"])

    def test_the_quota_and_the_volume_are_the_big_app_ones(self) -> None:
        from lazygui import osvolume
        self.assertEqual(catalog.build_env({**self.base(), "lazywebplus": True})["LAZYOS_LIMIT_QUOTA_USER_MEMORY"], "8G")
        self.assertTrue(osvolume.wants_webkit({"lazywebplus": True}))

    def test_session_modes_build_it_before_the_image(self) -> None:
        cfg = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "4G", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False, "script": 0, "lazywebplus": True}
        plan = catalog.build_plan(cfg)
        labels = [step["label"] for step in plan]
        at = labels.index("Build image (cargo build)")
        self.assertEqual(plan[at - 1]["argv"][1:], ["tools/xui/build.py"])
        self.assertIn("--net", plan[-1]["argv"])


if __name__ == "__main__":
    unittest.main()
