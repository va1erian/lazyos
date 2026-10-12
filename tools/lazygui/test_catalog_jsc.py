#!/usr/bin/env python3
"""Launcher tests for JavaScriptCore's `jsc` shell (`LAZYOS_JSC=1`, built in
Docker by `tools/jsc/build.py`): from the Advanced tab and run_demo.
`test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_jsc.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv  # noqa: E402


class JscTests(unittest.TestCase):
    """The launcher can put `jsc` and `cxxproof` in /system/bin, building them
    first, with or without the desktop."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_embed_variable(self) -> None:
        for desktop in (False, True):
            env = catalog.build_env({**self.base(), "desktop": desktop, "jsc": True})
            self.assertEqual(env["LAZYOS_JSC"], "1")
            self.assertNotIn("LAZYOS_JSC", catalog.build_env({**self.base(), "desktop": desktop}))

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--jsc", demo_argv(jsc=True, skip_build=False))
        self.assertNotIn("--jsc", demo_argv(skip_build=False))
        self.assertNotIn("--jsc", demo_argv(jsc=True, skip_build=True))

    def test_session_modes_build_it_before_the_image(self) -> None:
        cfg = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "2G", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False, "script": 0, "jsc": True}
        plan = catalog.build_plan(cfg)
        labels = [step["label"] for step in plan]
        at = labels.index("Build image (cargo build)")
        self.assertEqual(plan[at - 1]["argv"][1:], ["tools/jsc/build.py", "--require", "--jit"])
        env = plan[at].get("env", {})
        if env:
            self.assertEqual(env.get("LAZYOS_JSC"), "1")

    def test_it_builds_after_the_linux_programs(self) -> None:
        steps = catalog.app_steps({"linuxapps": True, "jsc": True, "tls": True})
        self.assertEqual([s["argv"][1] for s in steps],
                         ["tools/linuxapps/build.py", "tools/jsc/build.py",
                          "tools/nettls/build.py"])


if __name__ == "__main__":
    unittest.main()
