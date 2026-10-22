#!/usr/bin/env python3
"""Launcher tests for the developer tools (LAZYOS_DEVTOOLS=1, docs/devtool-packages-plan.md T4):
the Messenger Playground from the Simple tab's Developer tools group, the Advanced switch, run_demo
and its session. `test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_devtools.py
"""

from __future__ import annotations

import os
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


class DevToolsTests(unittest.TestCase):
    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_ships_them_on_the_desktop_only(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "devtools": True})
        self.assertEqual(env["LAZYOS_DEVTOOLS"], "1")
        self.assertNotIn("LAZYOS_DEVTOOLS", catalog.build_env({**self.base(), "desktop": True}))
        self.assertNotIn("LAZYOS_DEVTOOLS", catalog.build_env({**self.base(), "devtools": True}))

    def test_the_group_turns_them_on_for_a_desktop_only(self) -> None:
        on = catalog.group_switches({"devtools"})
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop", **on)
        self.assertTrue(cfg["devtools"])
        self.assertEqual(catalog.build_env({**self.base(), **cfg})["LAZYOS_DEVTOOLS"], "1")
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "CLI", **on)["devtools"])
        off = catalog.simple_config(demo_config(), "dev", "Desktop", **catalog.group_switches(set()))
        self.assertFalse(off["devtools"])
        self.assertIn("devtools", [key for key, *_ in catalog.SIMPLE_GROUPS])

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--devtools", demo_argv(devtools=True, desktop=True, skip_build=False))
        self.assertNotIn("--devtools", demo_argv(devtools=True, desktop=False, skip_build=False))
        self.assertNotIn("--devtools", demo_argv(devtools=True, desktop=True, skip_build=True))

    def test_the_full_suite_includes_them(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop", suite="full")
        self.assertTrue(cfg["devtools"])
        self.assertEqual(catalog.build_env(cfg)["LAZYOS_DEVTOOLS"], "1")
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "Desktop", suite="base")["devtools"])

    def test_the_build_modes_build_rhai_for_them(self) -> None:
        cfg = {**self.base(), **demo_config(mode="Build xui app"), "desktop": True, "devtools": True}
        labels = [step["label"] for step in catalog.build_plan(cfg)]
        self.assertTrue(any("rhai" in label for label in labels), labels)
        plain = [step["label"] for step in catalog.build_plan({**cfg, "devtools": False})]
        self.assertFalse(any("rhai" in label for label in plain), plain)

    def test_the_playground_session_asks_for_the_tool_and_builds_rhai(self) -> None:
        index = {entry[0]: i for i, entry in enumerate(catalog.SCRIPTS)}
        entry = catalog.SCRIPTS[index["xui_playground.json"]]
        self.assertEqual(entry[2:], (("desktop",), "playground"))
        cfg = {**self.base(), "mode": "Scripted session", "desktop": True, "skip_build": False,
               "xui_autostart": "playground", "script": index["xui_playground.json"],
               "profile": "dev", "accel": "auto", "memory": "1G", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False}
        self.assertEqual(catalog.build_env(cfg)["LAZYOS_DEVTOOLS"], "1")
        labels = [step["label"] for step in catalog.build_plan(cfg)]
        self.assertLess(next(i for i, label in enumerate(labels) if "rhai" in label),
                        labels.index("Build image (cargo build)"))

    def test_no_normal_session_launches_them(self) -> None:
        # Only `LAZYOS_DEVTOOLS=1` images ship them: `core_apps.json` must not wait for them.
        path = os.path.join(catalog.ROOT, "tools", "screenshot", "examples", "core_apps.json")
        with open(path, encoding="utf-8") as script:
            self.assertNotIn("os.lazy.playground", script.read())


if __name__ == "__main__":
    unittest.main()
