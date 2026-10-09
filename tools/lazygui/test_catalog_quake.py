#!/usr/bin/env python3
"""Launcher tests for the Quake package (id's shareware pak inside): from the
Simple tab, the Advanced tab and run_demo. `test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_quake.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


class QuakeTests(unittest.TestCase):
    """The launcher can put the Quake package (id's shareware pak inside) on
    the image (`/system/share/samples/quake.lzp`, then `pkgctl install`),
    from both tabs and run_demo."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_embed_variable(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "quake": True})
        self.assertEqual(env["LAZYOS_QUAKE"], "1")
        self.assertNotIn("LAZYOS_QUAKE", catalog.build_env({**self.base(), "desktop": True}))

    def test_simple_desktop_can_include_it_and_cli_cannot(self) -> None:
        self.assertTrue(catalog.simple_config(demo_config(), "dev", "Desktop", quake=True)["quake"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "CLI", quake=True)["quake"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "Desktop")["quake"])

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--quake", demo_argv(quake=True, skip_build=False))
        self.assertNotIn("--quake", demo_argv(skip_build=False))
        self.assertNotIn("--quake", demo_argv(quake=True, skip_build=True))

    def test_session_modes_build_the_package_before_the_image(self) -> None:
        cfg = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "1G", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False, "script": 0, "quake": True}
        plan = catalog.build_plan(cfg)
        labels = [step["label"] for step in plan]
        at = labels.index("Build image (cargo build)")
        self.assertEqual(labels[at - 1], "Build Quake package (engine + shareware pak)")
        self.assertEqual(plan[at - 1]["argv"][1:], ["tools/quake/build.py", "--require"])

    def test_it_builds_after_doom_and_before_emusic(self) -> None:
        steps = catalog.app_steps({"doom": True, "quake": True, "emusic": True})
        self.assertEqual([s["argv"][1] for s in steps],
                         ["tools/doom/build.py", "tools/quake/build.py",
                          "tools/emusic/build.py"])


if __name__ == "__main__":
    unittest.main()
