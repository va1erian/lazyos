#!/usr/bin/env python3
"""Tests for run_demo.py's WebKit flags: `--webkit-port` (the W4 console proof) and `--webkit` (the
browser, W5): switches, profile, memory and OS volume (no QEMU, no build).

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
        self.assertNotIn("LAZYOS_WEBKIT", self.builds[-1])
        self.assertEqual(self.builds[-1].get("LAZYOS_NETD"), "1")
        self.assertEqual(command[command.index("-m") + 1], "4G")
        self.assertEqual(self.builds[-1].get("LAZYOS_LIMIT_TASK_MAX"), "1024")

    def test_a_failed_webkit_port_build_stops_the_run(self) -> None:
        with mock.patch.object(run_demo, "build_webkit_port", return_value=False):
            code, command = self.run_main("--webkit-port", "--yes")
        self.assertNotEqual(code, 0)
        self.assertEqual(command, [])

    def test_webkit_is_a_desktop_app_with_the_stack_the_big_app_profile_and_a_large_volume(self) -> None:
        with mock.patch.object(run_demo, "build_webkit", return_value=True) as built, \
                mock.patch.object(run_demo.osvolume, "DEFAULT_IMAGE", self.dir / "none.img"), \
                mock.patch.object(run_demo, "build_xui_shell", return_value=True):
            code, command = self.run_main("--webkit", "--yes")
        self.assertEqual(code, 0)
        built.assert_called_once_with()
        env = self.builds[-1]
        self.assertEqual(env.get("LAZYOS_WEBKIT"), "1")
        self.assertEqual(env.get("LAZYOS_DESKTOP"), "1")
        self.assertEqual(env.get("LAZYOS_NETD"), "1")
        self.assertNotIn("LAZYOS_WEBKIT_PORT", env)
        self.assertEqual(command[command.index("-m") + 1], "4G")
        self.assertEqual(env.get("LAZYOS_LIMIT_TASK_MAX"), "1024")
        self.assertNotIn("LAZYOS_OS_SIZE", env, "a fresh image takes the build's own large default")

    def test_webkit_recreates_a_small_os_volume_with_a_gigabyte(self) -> None:
        from lazygui.test_osvolume import make_image
        small = make_image(self.dir, 512)
        with mock.patch.object(run_demo.osvolume, "DEFAULT_IMAGE", small), \
                mock.patch.object(run_demo, "build_webkit", return_value=True), \
                mock.patch.object(run_demo, "build_xui_shell", return_value=True):
            code, _ = self.run_main("--webkit", "--yes")
        self.assertEqual(code, 0)
        self.assertEqual(self.builds[-1]["LAZYOS_RESET_OS"], "1")
        self.assertEqual(self.builds[-1]["LAZYOS_OS_SIZE"], str(1024 << 20))

    def test_a_failed_webkit_build_stops_the_run(self) -> None:
        with mock.patch.object(run_demo, "build_webkit", return_value=False):
            code, command = self.run_main("--webkit", "--yes")
        self.assertNotEqual(code, 0)
        self.assertEqual(command, [])


if __name__ == "__main__":
    unittest.main()
