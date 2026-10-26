#!/usr/bin/env python3
"""Launcher tests for LazyVM's stage (`LAZYOS_LAZYVM=1`, docs/agents/lazyvm.md): from the
Advanced tab and run_demo. `test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_lazyvm.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv  # noqa: E402


class LazyVmTests(unittest.TestCase):
    """The launcher can put LazyVM's stage at /system/lazyvm, with or without the desktop."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_embed_variable(self) -> None:
        for desktop in (False, True):
            env = catalog.build_env({**self.base(), "desktop": desktop, "lazyvm": True})
            self.assertEqual(env["LAZYOS_LAZYVM"], "1")
            self.assertNotIn("LAZYOS_LAZYVM", catalog.build_env({**self.base(), "desktop": desktop}))

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--lazyvm", demo_argv(lazyvm=True, skip_build=False))
        self.assertNotIn("--lazyvm", demo_argv(skip_build=False))
        self.assertNotIn("--lazyvm", demo_argv(lazyvm=True, skip_build=True))


if __name__ == "__main__":
    unittest.main()
