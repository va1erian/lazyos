#!/usr/bin/env python3
"""Launcher tests for the WebKit dependency bundle's proof (`LAZYOS_WEBKIT_DEPS=1`,
`depsproof`, built in Docker by `tools/webkit/deps.py`): from the Advanced tab
and run_demo. `test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_webkit.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


class WebkitDepsTests(unittest.TestCase):
    """The launcher can put `depsproof` in /system/bin, building the bundle first."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_embed_variable(self) -> None:
        for desktop in (False, True):
            env = catalog.build_env({**self.base(), "desktop": desktop, "webkit_deps": True})
            self.assertEqual(env["LAZYOS_WEBKIT_DEPS"], "1")
            self.assertNotIn("LAZYOS_WEBKIT_DEPS", catalog.build_env({**self.base(), "desktop": desktop}))

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--webkit-deps", demo_argv(webkit_deps=True, skip_build=False))
        self.assertNotIn("--webkit-deps", demo_argv(skip_build=False))
        self.assertNotIn("--webkit-deps", demo_argv(webkit_deps=True, skip_build=True))

    def test_session_modes_build_it_before_the_image(self) -> None:
        cfg = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "2G", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False, "script": 0, "webkit_deps": True}
        plan = catalog.build_plan(cfg)
        labels = [step["label"] for step in plan]
        at = labels.index("Build image (cargo build)")
        self.assertEqual(plan[at - 1]["argv"][1:], ["tools/webkit/deps.py", "--require"])
        # Steps carry no env of their own: the runner applies build_env to the
        # whole plan, so that is where the switch must be.
        self.assertEqual(catalog.build_env({**self.base(), "webkit_deps": True}).get("LAZYOS_WEBKIT_DEPS"), "1")

    def test_the_gui_gives_it_networking_and_a_card(self) -> None:
        # depsproof's curl check fetches over HTTPS: the stack in the image and
        # a QEMU card must come with the checkbox, as run_demo --webkit-deps does.
        cfg = {**self.base(), "desktop": False, "webkit_deps": True}
        self.assertTrue(catalog.wants_net(cfg))
        env = catalog.build_env(cfg)
        self.assertEqual((env["LAZYOS_NETD"], env["LAZYOS_NETD_ARGS"]), ("1", "demo=0"))
        self.assertIn("--net", catalog.net_flags(cfg))
        # (No negative check: the launcher's always-on dbgd keeps networking on
        # for every build since #818, so the checkbox only has to not lose it.)
        session = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
                   "accel": "auto", "memory": "2G", "qemu": "", "out": "shots",
                   "timeout": "300", "tablet": False, "script": 0, "webkit_deps": True}
        plan = catalog.build_plan(session)
        self.assertIn("--net", plan[-1]["argv"])

    def test_it_builds_after_jsc(self) -> None:
        steps = catalog.app_steps({"jsc": True, "webkit_deps": True, "tls": True})
        self.assertEqual([s["argv"][1] for s in steps],
                         ["tools/jsc/build.py", "tools/webkit/deps.py", "tools/nettls/build.py"])


class WebkitPortTests(unittest.TestCase):
    """The launcher can put the WebKit port's console proof (W4) in /system/bin, building it first."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_embed_variable(self) -> None:
        for desktop in (False, True):
            env = catalog.build_env({**self.base(), "desktop": desktop, "webkit_port": True})
            self.assertEqual(env["LAZYOS_WEBKIT_PORT"], "1")
            self.assertNotIn("LAZYOS_WEBKIT_PORT", catalog.build_env({**self.base(), "desktop": desktop}))

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--webkit-port", demo_argv(webkit_port=True, skip_build=False))
        self.assertNotIn("--webkit-port", demo_argv(skip_build=False))
        self.assertNotIn("--webkit-port", demo_argv(webkit_port=True, skip_build=True))

    def test_it_builds_the_bundle_then_the_port(self) -> None:
        steps = catalog.app_steps({"webkit_port": True})
        self.assertEqual([s["argv"][1] for s in steps], ["tools/webkit/deps.py", "tools/webkit/port.py"])
        self.assertTrue(all("--require" in s["argv"] for s in steps))

    def test_session_modes_build_it_before_the_image(self) -> None:
        cfg = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "4G", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False, "script": 0, "webkit_port": True}
        plan = catalog.build_plan(cfg)
        labels = [step["label"] for step in plan]
        at = labels.index("Build image (cargo build)")
        self.assertEqual(plan[at - 1]["argv"][1:], ["tools/webkit/port.py", "--require"])
        self.assertEqual(plan[at].get("env", {}).get("LAZYOS_WEBKIT_PORT", "1"), "1")


if __name__ == "__main__":
    unittest.main()
