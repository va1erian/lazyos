#!/usr/bin/env python3
"""The background-start judge fails when it should (and passes a good log)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from background_check import judge  # noqa: E402

GOOD = """\
INIT:LAUNCH:PASS app=os.lazy.volume pid=30 session=1
INIT:LAUNCH:BACKGROUND app=os.lazy.volume pid=30 session=1
INIT:LAUNCH:PASS app=os.lazy.traydemo pid=31 session=1
INIT:LAUNCH:BACKGROUND app=os.lazy.traydemo pid=31 session=1
TRAYDEMO:WATCH:PASS
TRAYDEMO:BACKGROUND:PASS
SHELL:TRAY:SET app=os.lazy.traydemo
UI:RECT x=0 y=0 w=640 h=400 name=window:Terminal
TERM:UP:PASS
INIT:LAUNCH:EXISTING app=os.lazy.traydemo pid=31 session=1
TRAYDEMO:REOPEN:PASS via=reopen
UI:RECT x=10 y=10 w=460 h=220 name=window:Tray Demo
TRAYDEMO:UP:PASS
"""


class Judge(unittest.TestCase):
    def test_a_good_log_passes(self):
        self.assertEqual(judge(GOOD), [])

    def test_a_window_at_login_fails(self):
        log = GOOD.replace(
            "TRAYDEMO:BACKGROUND:PASS\n",
            "TRAYDEMO:BACKGROUND:PASS\nUI:RECT x=1 y=1 w=2 h=2 name=window:Tray Demo\n"
            "TRAYDEMO:UP:PASS\n",
        )
        self.assertEqual(len(judge(log)), 2)

    def test_a_foreground_autostart_fails(self):
        log = GOOD.replace("INIT:LAUNCH:BACKGROUND app=os.lazy.traydemo pid=31 session=1\n", "")
        log = log.replace("TRAYDEMO:BACKGROUND:PASS\n", "")
        failures = judge(log)
        self.assertIn("init did not start os.lazy.traydemo in the background", failures)
        self.assertIn("the demo did not report a background start", failures)

    def test_the_volume_applet_must_start_in_the_background_too(self):
        log = GOOD.replace("INIT:LAUNCH:BACKGROUND app=os.lazy.volume pid=30 session=1\n", "")
        self.assertEqual(judge(log), ["init did not start os.lazy.volume in the background"])

    def test_no_menu_launch_fails(self):
        log = GOOD.split("INIT:LAUNCH:EXISTING")[0]
        self.assertEqual(judge(log), ["the menu did not launch the running demo again"])

    def test_a_menu_launch_that_opens_no_window_fails(self):
        log = GOOD.replace("UI:RECT x=10 y=10 w=460 h=220 name=window:Tray Demo\n", "")
        self.assertEqual(judge(log), ["the demo's window did not open from the menu"])

    def test_a_missing_reopen_fails(self):
        log = GOOD.replace("TRAYDEMO:REOPEN:PASS via=reopen\n", "")
        self.assertEqual(judge(log), ["the menu launch did not reach the demo as a Reopen"])


if __name__ == "__main__":
    unittest.main()
