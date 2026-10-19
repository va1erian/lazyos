#!/usr/bin/env python3
"""Launcher tests for LazyGolf and the NES Player (LAZYOS_GAMES=1). `test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_games.py
"""

from __future__ import annotations

import os
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


class GamesTests(unittest.TestCase):
    """LazyGolf and the NES Player (LAZYOS_GAMES=1) from the Simple tab's Games
    group, the Advanced tab and run_demo."""

    def base(self) -> dict:
        return {"services": False, "xuid": False, "xui_client": False, "xui_app": "(none)",
                "shellprobe": False, "msgctl": False, "msgrd": False, "busybox": ""}

    def test_the_switch_ships_them_on_the_desktop_only(self) -> None:
        env = catalog.build_env({**self.base(), "desktop": True, "games": True})
        self.assertEqual(env["LAZYOS_GAMES"], "1")
        self.assertNotIn("LAZYOS_GAMES", catalog.build_env({**self.base(), "desktop": True}))
        self.assertNotIn("LAZYOS_GAMES", catalog.build_env({**self.base(), "games": True}))

    def test_the_games_group_turns_them_on(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop",
                                    **catalog.group_switches({"games"}))
        self.assertTrue(cfg["games"])
        self.assertEqual(catalog.build_env({**self.base(), **cfg})["LAZYOS_GAMES"], "1")
        off = catalog.simple_config(demo_config(), "dev", "Desktop", **catalog.group_switches(set()))
        self.assertFalse(off["games"])
        cli = catalog.simple_config(demo_config(), "dev", "CLI", **catalog.group_switches({"games"}))
        self.assertFalse(cli["games"])

    def test_the_demo_passes_the_run_demo_flag(self) -> None:
        self.assertIn("--games", demo_argv(games=True, desktop=True, skip_build=False))
        self.assertNotIn("--games", demo_argv(games=True, desktop=False, skip_build=False))
        self.assertNotIn("--games", demo_argv(games=True, desktop=True, skip_build=True))

    def test_their_sessions_ask_for_the_games(self) -> None:
        index = {entry[0]: i for i, entry in enumerate(catalog.SCRIPTS)}
        for name, app in (("xui_golf.json", "golf"), ("xui_nes.json", "term")):
            cfg = {**self.base(), "mode": "Scripted session", "desktop": True,
                   "xui_autostart": app, "script": index[name]}
            self.assertEqual(catalog.build_env(cfg)["LAZYOS_GAMES"], "1", name)

    def test_no_normal_session_launches_them(self) -> None:
        # Only `LAZYOS_GAMES=1` images ship them: `core_apps.json` must not wait for them.
        path = os.path.join(catalog.ROOT, "tools", "screenshot", "examples", "core_apps.json")
        with open(path, encoding="utf-8") as script:
            text = script.read()
        self.assertNotIn("os.lazy.golf", text)
        self.assertNotIn("os.lazy.nes", text)


if __name__ == "__main__":
    unittest.main()
