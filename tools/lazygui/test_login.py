#!/usr/bin/env python3
"""Launcher tests for the desktop's login (issue #623, docs/accounts-plan.md
U0): the Simple tab's "Log in automatically as user", the Advanced tab's
Autologin field and run_demo's `--autologin`. `test_catalog.py` runs them too.

Run: python tools/lazygui/test_login.py
"""

from __future__ import annotations

import argparse
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from lazygui import catalog, login  # noqa: E402
from lazygui.testplan import demo_argv, demo_config  # noqa: E402


def parse(argv: list[str]) -> argparse.Namespace:
    """run_demo's login option (plus `--no-build`) over ``argv``."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--no-build", action="store_true")
    login.add_login_option(parser)
    return parser.parse_args(argv)


class AutologinTests(unittest.TestCase):
    def test_names_are_checked_like_the_image_build(self) -> None:
        self.assertEqual(login.check_name(" user "), "user")
        self.assertEqual(login.check_name(""), "")
        self.assertEqual(login.check_name("none"), "none")
        for bad in ("User", "9lives", "a b", "../x", "x" * 40, "user;rm"):
            with self.assertRaises(ValueError, msg=bad):
                login.check_name(bad)

    def test_simple_desktop_logs_user_in_and_cli_cannot(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop", autologin=True)
        cfg["skip_build"] = False
        self.assertEqual(cfg["autologin"], "user")
        self.assertEqual(catalog.build_env(cfg)["LAZYOS_AUTOLOGIN"], "user")
        argv = catalog.build_plan(cfg)[-1]["argv"]
        self.assertEqual(argv[argv.index("--autologin") + 1], "user")
        cli = catalog.simple_config(demo_config(), "dev", "CLI", autologin=True)
        self.assertEqual(cli["autologin"], "")

    def test_off_by_default_the_login_screen(self) -> None:
        cfg = catalog.simple_config(demo_config(), "dev", "Desktop")
        self.assertEqual(cfg["autologin"], "")
        self.assertNotIn("LAZYOS_AUTOLOGIN", catalog.build_env(cfg))
        self.assertNotIn("--autologin", catalog.build_plan(cfg)[-1]["argv"])
        # run_demo without the flag asks the build for the login screen.
        self.assertEqual(login.build_login(parse([])), {"LAZYOS_AUTOLOGIN": "none"})

    def test_advanced_field_is_honoured_and_needs_a_build(self) -> None:
        argv = demo_argv(skip_build=False, autologin="admin")
        self.assertEqual(argv[argv.index("--autologin") + 1], "admin")
        self.assertNotIn("--autologin", demo_argv(skip_build=True, autologin="admin"))
        with self.assertRaises(ValueError):
            catalog.build_plan(demo_config(skip_build=False, autologin="Not A Name"))
        with self.assertRaises(ValueError):
            login.login_env({"autologin": "Not A Name"})

    def test_run_demo_flag(self) -> None:
        self.assertEqual(login.build_login(parse(["--autologin", "user"])),
                         {"LAZYOS_AUTOLOGIN": "user"})
        self.assertEqual(login.build_login(parse(["--no-build"])), {})
        for argv in (["--autologin", "user", "--no-build"], ["--autologin", "Bad"]):
            with self.assertRaises(ValueError, msg=argv):
                login.build_login(parse(argv))


if __name__ == "__main__":
    unittest.main()
