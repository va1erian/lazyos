#!/usr/bin/env python3
"""Tests for run_demo.py's opt-in desktop apps: each flag builds its artifacts
before the image and sets its switches (no QEMU, no build).

Run: python tools/test_run_demo_apps.py
"""

from __future__ import annotations

import sys
import unittest
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


if __name__ == "__main__":
    unittest.main()
