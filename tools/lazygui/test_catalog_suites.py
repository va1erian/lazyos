#!/usr/bin/env python3
"""Launcher tests for the software suites (`tools/suites.toml`): the Simple tab's
Software choice, the environment it builds and the `run_demo.py` flags it passes.
`test_catalog.py` runs them too.

Run: python tools/lazygui/test_catalog_suites.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog, osvolume  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


class SuiteTests(unittest.TestCase):
    def config(self, suite: str, interface: str = "Desktop", skip_build: bool = True, **more) -> dict:
        cfg = catalog.simple_config(demo_config(), "dev", interface, suite=suite, **more)
        return {**cfg, "skip_build": skip_build}

    def test_the_choices_are_custom_then_the_suites(self) -> None:
        self.assertEqual(catalog.software_choices(), ["Custom", "Base", "Full"])
        self.assertEqual(catalog.suite_name("Custom"), "")
        self.assertEqual(catalog.suite_name("Full"), "full")

    def test_full_turns_every_app_on_and_srb2kart_stays_off(self) -> None:
        cfg = self.config("full")
        for name in ("lazyrad", "modplayer", "pictures", "lazyweb", "webkit", "lazywebplus", "mail",
                     "lazysky", "games", "doom", "quake", "emusic", "linuxapps", "tls", "net",
                     "smb", "big_app"):
            self.assertTrue(cfg[name], name)
        self.assertFalse(cfg["srb2kart"])
        env = catalog.build_env(cfg)
        for switch in ("LAZYOS_DESKTOP", "LAZYOS_WEBKIT", "LAZYOS_LAZYWEBPLUS", "LAZYOS_GAMES",
                       "LAZYOS_QUAKE", "LAZYOS_SMB", "LAZYOS_NETD"):
            self.assertEqual(env[switch], "1", switch)
        self.assertNotIn("LAZYOS_SRB2KART", env)
        self.assertEqual(env["LAZYOS_LIMIT_FD_MAX"], "8192")  # the big-app profile
        self.assertEqual(env["LAZYOS_LIMIT_QUOTA_USER_MEMORY"], "8G")

    def test_base_is_the_desktop_alone(self) -> None:
        cfg = self.config("base")
        self.assertTrue(cfg["desktop"])
        self.assertFalse(cfg["webkit"] or cfg["net"] or cfg["doom"])
        self.assertNotIn("LAZYOS_WEBKIT", catalog.build_env(cfg))

    def test_the_srb2kart_option_adds_it_and_sizes_the_volume(self) -> None:
        cfg = self.config("full", suite_srb2kart=True)
        self.assertTrue(cfg["srb2kart"])
        self.assertEqual(catalog.build_env(cfg)["LAZYOS_SRB2KART"], "1")
        self.assertTrue(osvolume.wants_webkit(cfg))

    def test_a_suite_is_desktop_only(self) -> None:
        cfg = self.config("full", interface="CLI")
        self.assertNotIn("suite", cfg)
        self.assertFalse(cfg["webkit"])

    def test_the_demo_passes_the_suite_to_run_demo(self) -> None:
        self.assertEqual(self.argv(self.config("full", skip_build=False))[1:3], ["tools/run_demo.py", "--suite"])
        argv = self.argv(self.config("full", suite_srb2kart=True, skip_build=False))
        self.assertEqual(argv[argv.index("--suite") + 1], "full")
        self.assertEqual(argv[argv.index("--with") + 1], "srb2kart")
        self.assertNotIn("--suite", self.argv(self.config("base", skip_build=True)))
        self.assertNotIn("--suite", demo_argv(skip_build=False))

    def argv(self, cfg: dict) -> list[str]:
        return catalog.build_plan(cfg)[-1]["argv"]


if __name__ == "__main__":
    unittest.main()
