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

    def test_the_switch_sets_the_build_variable_and_the_stack(self) -> None:
        for desktop in (False, True):
            env = catalog.build_env({**self.base(), "desktop": desktop, "dbgd": True})
            self.assertEqual(env["LAZYOS_DBGD"], "1")
            self.assertEqual(env["LAZYOS_NETD"], "1", "dbgd listens through the socket service")
            self.assertNotIn("LAZYOS_DBGD", catalog.build_env({**self.base(), "desktop": desktop}))

    def test_the_card_and_the_port_come_with_it(self) -> None:
        self.assertEqual(catalog.net_flags({"net": False, "dbgd": False}), [])
        flags = catalog.net_flags({"net": False, "dbgd": True})
        self.assertIn("--net", flags)
        self.assertIn("9701:9701", flags)
        typed = catalog.net_flags({"net": True, "dbgd": True, "net_forwards": "8080:8080"})
        self.assertNotIn("9701:9701", typed, "a typed forward list is the user's")

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--dbgd", demo_argv(dbgd=True, skip_build=False))
        self.assertNotIn("--dbgd", demo_argv(skip_build=False))
        self.assertNotIn("--dbgd", demo_argv(dbgd=True, skip_build=True))

    def test_the_control_tier_implies_dbgd_and_is_off_by_default(self) -> None:
        plain = catalog.build_env({**self.base(), "desktop": False, "dbgd": True})
        self.assertNotIn("LAZYOS_DBGD_CONTROL", plain)
        env = catalog.build_env({**self.base(), "desktop": False, "dbgd_control": True})
        self.assertEqual(env["LAZYOS_DBGD"], "1", "control implies the service")
        self.assertEqual(env["LAZYOS_DBGD_CONTROL"], "1")
        self.assertEqual(env["LAZYOS_NETD"], "1")
        self.assertIn("9701:9701", catalog.net_flags({"net": False, "dbgd_control": True}))

    def test_the_demo_passes_the_control_flag(self) -> None:
        argv = demo_argv(dbgd_control=True, skip_build=False)
        self.assertIn("--dbgd-control", argv)
        self.assertNotIn("--dbgd", argv, "--dbgd-control implies it")
        self.assertNotIn("--dbgd-control", demo_argv(dbgd=True, skip_build=False))


if __name__ == "__main__":
    unittest.main()
