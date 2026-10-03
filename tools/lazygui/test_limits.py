#!/usr/bin/env python3
"""Tests for the launcher's guest memory default and kernel limits (`LAZYOS_LIMIT_*`).

Run: python tools/lazygui/test_limits.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.test_catalog import demo_argv, demo_config  # noqa: E402


class MemoryAndLimitsTests(unittest.TestCase):
    """1 GiB guests by default, and kernel limits as `LAZYOS_LIMIT_*` build switches."""

    def env(self, **overrides) -> dict[str, str]:
        cfg = {"desktop": True, "services": False, "xuid": False, "xui_client": False,
               "xui_app": "(none)", "shellprobe": False, "msgctl": False, "msgrd": False,
               "busybox": ""}
        cfg.update(overrides)
        return catalog.build_env(cfg)

    def test_the_gui_default_matches_every_launcher(self) -> None:
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "screenshot"))
        import qemu_qmp  # noqa: E402

        self.assertEqual(catalog.DEFAULT_MEMORY, "1G")
        self.assertEqual(qemu_qmp.DEFAULT_MEMORY, catalog.DEFAULT_MEMORY)
        argv = qemu_qmp.build_qemu_command("qemu", None, 1, Path("serial.log"))
        self.assertEqual(argv[argv.index("-m") + 1], "1G")

    def test_the_memory_reaches_the_demo(self) -> None:
        argv = demo_argv(memory="4G")
        self.assertEqual(argv[argv.index("--memory") + 1], "4G")

    def test_limits_become_build_variables(self) -> None:
        env = self.env(limits="heap_max=512M  fd_max=4096")
        self.assertEqual(env["LAZYOS_LIMIT_HEAP_MAX"], "512M")
        self.assertEqual(env["LAZYOS_LIMIT_FD_MAX"], "4096")
        self.assertFalse(any(key.startswith("LAZYOS_LIMIT_") for key in self.env()))

    def test_limit_keys_match_the_kernel(self) -> None:
        source = (Path(catalog.ROOT) / "kernel" / "src" / "limits.rs").read_text(encoding="utf-8")
        for key in catalog.LIMIT_KEYS:
            self.assertIn(f'name: "{key}"', source)
        self.assertEqual(source.count("name: \""), len(catalog.LIMIT_KEYS))

    def test_bad_limits_are_refused(self) -> None:
        for text in ("heap_max", "nope=1", "heap_max=", "=5"):
            with self.assertRaises(ValueError):
                catalog.limit_env(text)
        self.assertEqual(catalog.limit_env(["STACK_SIZE=16M"]), {"LAZYOS_LIMIT_STACK_SIZE": "16M"})

    def test_limits_need_a_build(self) -> None:
        with self.assertRaises(ValueError):
            catalog.build_plan(demo_config(skip_build=True, limits="fd_max=4096"))
        steps = catalog.build_plan(demo_config(skip_build=False, limits="fd_max=4096"))
        self.assertNotIn("--no-build", steps[-1]["argv"])

    def test_bad_limits_fail_the_plan_and_the_image_build(self) -> None:
        with self.assertRaises(ValueError):
            catalog.build_plan(demo_config(skip_build=False, limits="heap_max"))
        cfg = demo_config(skip_build=False, limits="heap_max", desktop=True, services=False,
                          xuid=False, xui_client=False, xui_app="(none)", shellprobe=False,
                          msgctl=False, msgrd=False, busybox="")
        with self.assertRaises(ValueError):
            catalog.image_build(cfg)

    def test_simple_mode_drops_advanced_limits(self) -> None:
        cfg = catalog.simple_config(demo_config(limits="heap_max=512M"), "dev", "Desktop")
        self.assertEqual(cfg["limits"], "")
        self.assertFalse(any(k.startswith("LAZYOS_LIMIT_") for k in catalog.build_env(cfg)))


if __name__ == "__main__":
    unittest.main()
