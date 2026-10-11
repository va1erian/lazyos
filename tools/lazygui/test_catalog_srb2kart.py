#!/usr/bin/env python3
"""Launcher tests for the SRB2Kart (package plus fetched game data): from the
Simple tab, the Advanced tab and run_demo. `test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_srb2kart.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


class Srb2kartTests(unittest.TestCase):
    """The launcher can put the SRB2Kart (package plus fetched game data) on
    the image (`/system/share/samples/srb2kart.lzp`, then `pkgctl install`),
    from both tabs and run_demo."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_embed_variable(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "srb2kart": True})
        self.assertEqual(env["LAZYOS_SRB2KART"], "1")
        self.assertNotIn("LAZYOS_SRB2KART", catalog.build_env({**self.base(), "desktop": True}))

    def test_simple_desktop_can_include_it_and_cli_cannot(self) -> None:
        self.assertTrue(catalog.simple_config(demo_config(), "dev", "Desktop", srb2kart=True)["srb2kart"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "CLI", srb2kart=True)["srb2kart"])
        self.assertFalse(catalog.simple_config(demo_config(), "dev", "Desktop")["srb2kart"])

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--srb2kart", demo_argv(srb2kart=True, skip_build=False))
        self.assertNotIn("--srb2kart", demo_argv(skip_build=False))
        self.assertNotIn("--srb2kart", demo_argv(srb2kart=True, skip_build=True))

    def test_session_modes_build_the_package_before_the_image(self) -> None:
        cfg = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "1G", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False, "script": 0, "srb2kart": True}
        plan = catalog.build_plan(cfg)
        labels = [step["label"] for step in plan]
        at = labels.index("Build image (cargo build)")
        self.assertEqual(labels[at - 1], "Build SRB2Kart (engine package + fetched game data)")
        self.assertEqual(plan[at - 1]["argv"][1:], ["tools/srb2kart/build.py", "--require"])

    def test_it_builds_after_doom_and_before_emusic(self) -> None:
        steps = catalog.app_steps({"doom": True, "srb2kart": True, "emusic": True})
        self.assertEqual([s["argv"][1] for s in steps],
                         ["tools/doom/build.py", "tools/srb2kart/build.py",
                          "tools/emusic/build.py"])

    def test_simple_tab_has_an_option_for_it(self) -> None:
        self.assertIn("srb2kart", catalog.SIMPLE_OPTIONS)
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop", srb2kart=True)
        self.assertEqual(catalog.build_env(cfg)["LAZYOS_SRB2KART"], "1")


if __name__ == "__main__":
    unittest.main()
