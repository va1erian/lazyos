#!/usr/bin/env python3
"""Tests for run_demo.py's opt-in desktop apps: each flag builds its artifacts
before the image and sets its switches (no QEMU, no build).

Run: python tools/test_run_demo_apps.py
"""

from __future__ import annotations

import io
import sys
import tempfile
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import run_demo  # noqa: E402
from test_run_demo import RunDemoCase  # noqa: E402


class OptInAppTests(RunDemoCase):
    """`--emusic`, `--modplayer` and `--pictures`."""

    def test_emusic_brings_the_desktop_and_a_sound_card(self) -> None:
        with mock.patch.object(run_demo, "build_emusic", return_value=True) as package, \
                mock.patch.object(run_demo, "build_xui_shell", return_value=True):
            code, command = self.run_main("--emusic")
        self.assertEqual(code, 0)
        package.assert_called_once()
        env = self.builds[-1]
        for switch in ("LAZYOS_EMUSIC", "LAZYOS_DESKTOP", "LAZYOS_SOUND"):
            self.assertEqual(env.get(switch), "1", switch)
        self.assertIn("virtio-sound-pci,audiodev=snd0", command)

    def test_srb2kart_builds_the_package_and_brings_the_desktop(self) -> None:
        with mock.patch.object(run_demo, "build_srb2kart", return_value=True) as package, \
                mock.patch.object(run_demo, "build_xui_shell", return_value=True):
            code, _command = self.run_main("--srb2kart")
        self.assertEqual(code, 0)
        package.assert_called_once()
        for switch in ("LAZYOS_SRB2KART", "LAZYOS_DESKTOP"):
            self.assertEqual(self.builds[-1].get(switch), "1", switch)

    def test_modplayer_brings_lazyrad_the_desktop_and_a_sound_card(self) -> None:
        with mock.patch.object(run_demo, "build_lazyrad", return_value=True) as lazyrad, \
                mock.patch.object(run_demo, "build_modplayer", return_value=True) as package, \
                mock.patch.object(run_demo, "build_xui_shell", return_value=True):
            code, command = self.run_main("--modplayer")
        self.assertEqual(code, 0)
        lazyrad.assert_called_once()
        package.assert_called_once()
        env = self.builds[-1]
        for switch in ("LAZYOS_MODPLAYER", "LAZYOS_LAZYRAD", "LAZYOS_DESKTOP", "LAZYOS_SOUND"):
            self.assertEqual(env.get(switch), "1", switch)
        self.assertIn("virtio-sound-pci,audiodev=snd0", command)

    def test_pictures_is_a_desktop_app_whose_player_is_built_before_packaging(self) -> None:
        order: list[str] = []
        with mock.patch.object(run_demo, "build_xui_shell", return_value=True), \
                mock.patch.object(run_demo, "build_pictures",
                                  side_effect=lambda: order.append("player") or True), \
                mock.patch.object(run_demo, "build_core_packages",
                                  side_effect=lambda: order.append("packages") or True):
            self.assertEqual(self.run_main("--pictures")[0], 0)
        self.assertEqual(order, ["player", "packages"])
        self.assertEqual(self.builds[-1].get("LAZYOS_PICTURES"), "1")
        self.assertEqual(self.builds[-1].get("LAZYOS_DESKTOP"), "1")

    def test_installer_builds_it_and_writes_the_stick(self) -> None:
        with mock.patch.object(run_demo, "build_installer", return_value=True) as build:
            self.assertEqual(self.run_main("--installer")[0], 0)
        build.assert_called_once()
        env = self.builds[-1]
        for switch in ("LAZYOS_INSTALLER", "LAZYOS_USB_IMAGE", "LAZYOS_USB"):
            self.assertEqual(env.get(switch), "1", switch)

    def test_a_failed_installer_build_stops_the_run(self) -> None:
        with mock.patch.object(run_demo, "build_installer", return_value=False):
            self.assertEqual(self.run_main("--installer")[0], 1)
        self.assertEqual(self.builds, [])


class NetbootTests(RunDemoCase):
    """`--netboot` and `--netboot-key`: the stick's profile rule, plus the payload switch."""

    def test_netboot_sets_the_payload_and_stick_switches(self) -> None:
        code, _command = self.run_main("--netboot")
        self.assertEqual(code, 0)
        env = self.builds[-1]
        for switch in ("LAZYOS_NETBOOT", "LAZYOS_USB", "LAZYOS_SERVICES"):
            self.assertEqual(env.get(switch), "1", switch)
        self.assertNotIn("LAZYOS_NETBOOT_KEY", env)
        self.assertNotIn("LAZYOS_USB_IMAGE", env)

    def test_netboot_on_the_desktop_needs_no_services_switch(self) -> None:
        with mock.patch.object(run_demo, "build_xui_shell", return_value=True):
            code, _command = self.run_main("--desktop", "--netboot")
        self.assertEqual(code, 0)
        env = self.builds[-1]
        self.assertEqual(env.get("LAZYOS_NETBOOT"), "1")
        self.assertEqual(env.get("LAZYOS_DESKTOP"), "1")
        self.assertNotIn("LAZYOS_SERVICES", env)

    def test_the_key_is_passed_as_an_absolute_path(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            key = Path(directory) / "key.sec"
            key.write_text("00" * 32 + "\n")
            code, _command = self.run_main("--netboot", "--netboot-key", str(key))
        self.assertEqual(code, 0)
        self.assertEqual(self.builds[-1].get("LAZYOS_NETBOOT_KEY"), str(key.resolve()))

    def test_a_key_without_netboot_or_a_missing_key_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            key = Path(directory) / "key.sec"
            key.write_text("00" * 32 + "\n")
            for argv in (["--netboot-key", str(key)],
                         ["--netboot", "--netboot-key", str(Path(directory) / "missing")]):
                with self.assertRaises(SystemExit), redirect_stderr(io.StringIO()):
                    run_demo.main(argv + ["--build-only"])
        self.assertEqual(self.builds, [])

    def test_without_the_flag_nothing_is_set(self) -> None:
        self.run_main()
        self.assertNotIn("LAZYOS_NETBOOT", self.builds[-1])


if __name__ == "__main__":
    unittest.main()
