#!/usr/bin/env python3
"""Launcher tests for the remote inspection service `dbgd` (docs/dbgd-plan.md,
issue #701): the Advanced tab switch, run_demo's `--dbgd` and the port forward.
`test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_dbgd.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


class DbgdTests(unittest.TestCase):
    """The remote inspection service `dbgd` (docs/dbgd-plan.md): its switch,
    the stack and port forward it needs, and the run_demo flag."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_dbgd_is_always_built_in_with_the_stack(self) -> None:
        for desktop in (False, True):
            env = catalog.build_env({**self.base(), "desktop": desktop})
            self.assertEqual(env["LAZYOS_DBGD"], "1")
            self.assertEqual(env["LAZYOS_DBGD_CONTROL"], "1")
            self.assertEqual(env["LAZYOS_NETD"], "1", "dbgd listens through the socket service")

    def test_register_reads_are_their_own_switch(self) -> None:
        off = catalog.build_env({**self.base(), "desktop": True})
        self.assertNotIn("LAZYOS_DBGD_MMIO", off, "a register read can wedge hardware")
        on = catalog.build_env({**self.base(), "desktop": True, "dbgd_mmio": True})
        self.assertEqual(on["LAZYOS_DBGD_MMIO"], "1")
        self.assertEqual(on["LAZYOS_DBGD"], "1")

    def test_claimed_reads_and_ranges_are_advanced_controls(self) -> None:
        cfg = {**self.base(), "desktop": True, "dbgd_mmio_claimed": True,
               "dbgd_mmio_allow": "8086:5916:0:0x45400-0x45410 1af4:1050:0:0-0x100"}
        env = catalog.build_env(cfg)
        self.assertEqual(env["LAZYOS_DBGD_MMIO"], "1", "they mean nothing without the tier")
        self.assertEqual(env["LAZYOS_DBGD_MMIO_CLAIMED"], "1")
        self.assertEqual(env["LAZYOS_DBGD_MMIO_ALLOW"],
                         "8086:5916:0:0x45400-0x45410,1af4:1050:0:0-0x100")
        quiet = catalog.build_env({**self.base(), "desktop": True})
        self.assertNotIn("LAZYOS_DBGD_MMIO_CLAIMED", quiet)
        self.assertNotIn("LAZYOS_DBGD_MMIO_ALLOW", quiet)
        argv = catalog.dbgd_argv(cfg)
        self.assertIn("--dbgd-mmio", argv)
        self.assertIn("--dbgd-mmio-claimed", argv)
        self.assertEqual(argv.count("--dbgd-mmio-allow"), 2)
        self.assertEqual(argv[argv.index("--dbgd-mmio-allow") + 1], "8086:5916:0:0x45400-0x45410")

    def test_the_card_and_the_port_come_with_it(self) -> None:
        flags = catalog.net_flags({"net": False})
        self.assertIn("--net", flags)
        self.assertIn("9701:9701", flags)
        typed = catalog.net_flags({"net": True, "net_forwards": "8080:8080"})
        self.assertNotIn("9701:9701", typed, "a typed forward list is the user's")

    def test_the_demo_passes_the_control_flag(self) -> None:
        argv = demo_argv(skip_build=False)
        self.assertIn("--dbgd-control", argv)
        self.assertNotIn("--dbgd", argv, "--dbgd-control implies it")
        self.assertNotIn("--dbgd-control", demo_argv(skip_build=True))


if __name__ == "__main__":
    unittest.main()
