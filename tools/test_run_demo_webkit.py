#!/usr/bin/env python3
"""Tests for run_demo.py's `--webkit-port` flag (the W4 console proof): switch, profile, memory and OS
volume (no QEMU, no build).

Run: python tools/test_run_demo_webkit.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run_demo  # noqa: E402
# A module import, so unittest does not collect MainTests a second time here.
import test_run_demo  # noqa: E402


class WebkitTests(test_run_demo.RunDemoCase):
    """The faked build and QEMU of `RunDemoCase`."""

    def test_webkit_port_builds_the_port_sets_the_switch_and_brings_the_network(self) -> None:
        with mock.patch.object(run_demo, "build_webkit_port", return_value=True) as built:
            code, command = self.run_main("--webkit-port", "--yes")
        self.assertEqual(code, 0)
        built.assert_called_once_with()
        self.assertEqual(self.builds[-1].get("LAZYOS_WEBKIT_PORT"), "1")
        self.assertNotIn("LAZYOS_LAZYWEBPLUS", self.builds[-1])
        self.assertEqual(self.builds[-1].get("LAZYOS_NETD"), "1")
        self.assertEqual(command[command.index("-m") + 1], "4G")
        self.assertEqual(self.builds[-1].get("LAZYOS_LIMIT_TASK_MAX"), "1024")

    def test_a_failed_webkit_port_build_stops_the_run(self) -> None:
        with mock.patch.object(run_demo, "build_webkit_port", return_value=False):
            code, command = self.run_main("--webkit-port", "--yes")
        self.assertNotEqual(code, 0)
        self.assertEqual(command, [])

    def test_webd_builds_it_sets_the_switch_and_wants_no_network(self) -> None:
        with mock.patch.object(run_demo, "build_webd", return_value=True) as built,                 mock.patch.object(run_demo, "build_xui_shell", return_value=True):
            code, command = self.run_main("--webd", "--yes")
        self.assertEqual(code, 0)
        built.assert_called_once_with()
        self.assertEqual(self.builds[-1].get("LAZYOS_WEBD"), "1")
        self.assertEqual(self.builds[-1].get("LAZYOS_DESKTOP"), "1")
        # A light view has no network: the stack is not asked for.
        self.assertNotIn("LAZYOS_NETD", self.builds[-1])
        self.assertEqual(command[command.index("-m") + 1], "4G")
        self.assertEqual(self.builds[-1].get("LAZYOS_LIMIT_TASK_MAX"), "1024")
        self.assertEqual(self.builds[-1].get("LAZYOS_LIMIT_QUOTA_USER_MEMORY"), "8G")

    def test_a_failed_webd_build_stops_the_run(self) -> None:
        with mock.patch.object(run_demo, "build_webd", return_value=False),                 mock.patch.object(run_demo, "build_xui_shell", return_value=True):
            code, command = self.run_main("--webd", "--yes")
        self.assertNotEqual(code, 0)
        self.assertEqual(command, [])

if __name__ == "__main__":
    unittest.main()
