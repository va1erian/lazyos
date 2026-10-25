#!/usr/bin/env python3
"""Launcher tests for the Rust compiler (`LAZYOS_RUSTC=1`, cross-compiled in
Docker by `tools/selfhost/build.py`, docs/self-hosting-plan.md): from the
Advanced tab and run_demo, and the OS volume it needs. `test_catalog.py`
runs them too.

Run: python tools/lazygui/test_catalog_rustc.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog, osvolume  # noqa: E402
from lazygui.testplan import demo_argv  # noqa: E402


class RustcTests(unittest.TestCase):
    """The launcher can put rustc in /system/bin, building it first, with or
    without the desktop, on a volume with room for it."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_sets_the_embed_variable(self) -> None:
        for desktop in (False, True):
            env = catalog.build_env({**self.base(), "desktop": desktop, "rustc": True})
            self.assertEqual(env["LAZYOS_RUSTC"], "1")
            self.assertNotIn("LAZYOS_RUSTC", catalog.build_env({**self.base(), "desktop": desktop}))

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--rustc", demo_argv(rustc=True, skip_build=False))
        self.assertNotIn("--rustc", demo_argv(skip_build=False))
        self.assertNotIn("--rustc", demo_argv(rustc=True, skip_build=True))

    def test_session_modes_build_it_before_the_image(self) -> None:
        cfg = {"mode": "Scripted session", "profile": "dev", "skip_build": False,
               "accel": "auto", "memory": "2G", "qemu": "", "out": "shots",
               "timeout": "300", "tablet": False, "script": 0, "rustc": True}
        plan = catalog.build_plan(cfg)
        labels = [step["label"] for step in plan]
        at = labels.index("Build image (cargo build)")
        self.assertEqual(plan[at - 1]["argv"][1:], ["tools/selfhost/build.py", "--require"])

    def test_it_adds_a_gib_to_the_os_volume(self) -> None:
        self.assertEqual(osvolume.required_bytes(False, False, True),
                         osvolume.DEFAULT_SIZE + osvolume.RUSTC_EXTRA)
        self.assertEqual(osvolume.RUSTC_EXTRA, 1 << 30)
        self.assertEqual(osvolume.required_bytes(True, True, True),
                         osvolume.required_bytes(True, True) + osvolume.RUSTC_EXTRA)


if __name__ == "__main__":
    unittest.main()
