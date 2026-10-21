#!/usr/bin/env python3
"""Launcher tests for webd (`LAZYOS_WEBD=1`, docs/webd.md): the daemon that serves WebKit light views, from the
Advanced tab and run_demo. `test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_webd.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog, osvolume  # noqa: E402
from lazygui.testplan import demo_argv  # noqa: E402


class WebdTests(unittest.TestCase):
    """A resident desktop app built in Docker (the bundle, the port's processes, the daemon) and packaged by the
    xui build. A light view has no network, so the switch does not ask for the stack (the launcher's dev profile
    has it anyway, for `dbgd`)."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_brings_the_desktop(self) -> None:
        env = catalog.build_env({**self.base(), "webd": True})
        self.assertEqual(env.get("LAZYOS_WEBD"), "1")
        self.assertEqual(env.get("LAZYOS_DESKTOP"), "1")
        self.assertNotIn("LAZYOS_WEBD", catalog.build_env({**self.base(), "desktop": True}))

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--webd", demo_argv(webd=True, skip_build=False))
        self.assertNotIn("--webd", demo_argv(skip_build=False))
        self.assertNotIn("--webd", demo_argv(webd=True, skip_build=True))

    def test_it_builds_the_bundle_the_port_the_daemon_then_packages_it(self) -> None:
        steps = catalog.app_steps({"webd": True})
        self.assertEqual([s["argv"][1] for s in steps],
                         ["tools/webkit/deps.py", "tools/webkit/port.py", "tools/webkit/webd.py", "tools/xui/build.py"])
        self.assertEqual(steps[1]["argv"][2:], ["--require", "--only", "WebProcess", "NetworkProcess"])
        self.assertEqual(catalog.app_steps({}), [])

    def test_the_quota_and_the_volume_are_the_webkit_ones(self) -> None:
        env = catalog.build_env({**self.base(), "webd": True})
        self.assertEqual(env["LAZYOS_LIMIT_QUOTA_USER_MEMORY"], "8G")
        self.assertTrue(osvolume.wants_webkit({"webd": True}))
        self.assertFalse(osvolume.wants_webkit({}))

    def test_session_modes_build_it_before_the_image(self) -> None:
        cfg = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "4G", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False, "script": 0, "webd": True}
        plan = catalog.build_plan(cfg)
        labels = [step["label"] for step in plan]
        at = labels.index("Build image (cargo build)")
        self.assertEqual(plan[at - 1]["argv"][1:], ["tools/xui/build.py"])


if __name__ == "__main__":
    unittest.main()
