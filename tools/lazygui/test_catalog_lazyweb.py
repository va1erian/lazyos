#!/usr/bin/env python3
"""Launcher tests for LazyWeb (`LAZYOS_LAZYWEB=1`, docs/lazyweb.md): the WebKit browser, from the Simple and Advanced tabs and run_demo. `test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_lazyweb.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


class LazyWebTests(unittest.TestCase):
    """A desktop app that brings the desktop profile and the network stack and is a client of webd:
    webd is built in Docker (the bundle, the port's processes, the program), the browser by the xui build."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_brings_the_desktop_and_the_stack(self) -> None:
        env = catalog.build_env({**self.base(), "lazyweb": True})
        for name in ("LAZYOS_LAZYWEB", "LAZYOS_DESKTOP", "LAZYOS_NETD"):
            self.assertEqual(env.get(name), "1", name)
        self.assertNotIn("LAZYOS_LAZYWEB", catalog.build_env({**self.base(), "desktop": True}))

    def test_simple_desktop_offers_it_and_cli_does_not(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop", lazyweb=True)
        self.assertTrue(cfg["lazyweb"] and cfg["net"])
        cli = catalog.simple_config(demo_config(), "dev", "CLI", lazyweb=True)
        self.assertFalse(cli["lazyweb"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "Desktop")["lazyweb"])

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--lazyweb", demo_argv(lazyweb=True, skip_build=False))
        self.assertNotIn("--lazyweb", demo_argv(skip_build=False))
        self.assertNotIn("--lazyweb", demo_argv(lazyweb=True, skip_build=True))
        self.assertIn("--net", demo_argv(lazyweb=True, skip_build=False))

    def test_the_ad_blockers_updates_bring_the_https_tools(self) -> None:
        """The blocker fetches its lists with `curl` (a child under its own label, issue #845)."""
        env = catalog.build_env({**self.base(), "lazyweb": True})
        self.assertEqual(env.get("LAZYOS_TLS"), "1")
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop", lazyweb=True)
        self.assertTrue(cfg["tls"])
        self.assertIn("--tls", demo_argv(lazyweb=True, tls=True, skip_build=False))
        steps = catalog.app_steps({"lazyweb": True, "tls": True})
        self.assertIn("tools/nettls/build.py", [s["argv"][1] for s in steps])

    def test_it_builds_the_bundle_the_port_and_webd_then_packages_the_apps(self) -> None:
        steps = catalog.app_steps({"lazyweb": True})
        self.assertEqual([s["argv"][1] for s in steps],
                         ["tools/webkit/deps.py", "tools/webkit/port.py", "tools/webkit/webd.py", "tools/xui/build.py"])
        self.assertNotIn("tools/webkit/app.py", [s["argv"][1] for s in steps])
        # Asking for webd as well builds it once.
        both = catalog.app_steps({"lazyweb": True, "webd": True})
        self.assertEqual([s["argv"][1] for s in both], [s["argv"][1] for s in steps])
        self.assertEqual(steps[1]["argv"][2:], ["--require", "--only", "WebProcess", "NetworkProcess"])

    def test_webd_is_not_left_out_of_a_browser_image(self) -> None:
        """`no_webd` is ignored with the browser: `run_demo` refuses `--no-webd --lazyweb`."""
        from lazygui import osvolume
        cfg = {"lazyweb": True, "no_webd": True}
        self.assertTrue(osvolume.wants_webd(cfg))
        self.assertNotIn("LAZYOS_WEBD", catalog.desktop_app_env(cfg))
        self.assertNotIn("--no-webd", catalog.desktop_app_argv(cfg))
        self.assertIn("--lazyweb", catalog.desktop_app_argv(cfg))
        self.assertIn("--no-webd", catalog.desktop_app_argv({"desktop": True, "no_webd": True}))

    def test_the_quota_and_the_volume_are_the_big_app_ones(self) -> None:
        from lazygui import osvolume
        self.assertEqual(catalog.build_env({**self.base(), "lazyweb": True})["LAZYOS_LIMIT_QUOTA_USER_MEMORY"], "8G")
        self.assertTrue(osvolume.wants_webkit({"lazyweb": True}))

    def test_session_modes_build_it_before_the_image(self) -> None:
        cfg = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "4G", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False, "script": 0, "lazyweb": True}
        plan = catalog.build_plan(cfg)
        labels = [step["label"] for step in plan]
        at = labels.index("Build image (cargo build)")
        self.assertEqual(plan[at - 1]["argv"][1:], ["tools/xui/build.py"])
        self.assertIn("--net", plan[-1]["argv"])


if __name__ == "__main__":
    unittest.main()
