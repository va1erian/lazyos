"""The grab judge fails when it should: python tools/input/test_verify_grab.py"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import verify_grab  # noqa: E402

GOOD = """\
DOOM:UP:PASS mode=window
DOOM:KEYSTATE:PASS
SHELL:MENU:OPEN
SHELL:MENU:CLOSE
INPUTD:GRAB:REQUEST session=4 surface=12
XUID:GRAB:ASK surface=12 allow=1
INPUTD:GRAB:ON session=4 surface=12
DOOM:GRAB:ON
XUID:GRAB:HELD surface=12
INPUTD:GRAB:ESCAPE
INPUTD:GRAB:OFF session=4 reason=4
XUID:GRAB:ESCAPE
DOOM:GRAB:OFF reason=4
XUID:GRAB:HELD surface=none
SHELL:MENU:OPEN
SHELL:MENU:CLOSE
"""


class JudgeTests(unittest.TestCase):
    def test_the_good_log_passes(self):
        self.assertEqual(verify_grab.judge(GOOD), [])

    def test_markers_interleaved_mid_line_still_count(self):
        log = GOOD.replace("DOOM:GRAB:ON\n", "garbage DOOM:GRAB:ON\n")
        self.assertEqual(verify_grab.judge(log), [])

    def test_a_start_menu_under_the_grab_fails(self):
        log = GOOD.replace("DOOM:GRAB:ON\n", "DOOM:GRAB:ON\nSHELL:MENU:OPEN\n")
        self.assertTrue(any("under the grab" in f for f in verify_grab.judge(log)))

    def test_a_denied_grab_fails(self):
        log = GOOD.replace("allow=1", "allow=0")
        self.assertTrue(verify_grab.judge(log))

    def test_no_escape_fails(self):
        log = GOOD.replace("INPUTD:GRAB:ESCAPE\n", "")
        self.assertTrue(verify_grab.judge(log))

    def test_a_grab_taken_back_fails(self):
        log = GOOD + "INPUTD:GRAB:ON session=4 surface=12\n"
        self.assertTrue(any("taken back" in f for f in verify_grab.judge(log)))

    def test_label_deny_fails(self):
        log = GOOD + "LABEL:DENY label=app:org.lazy.doom iface=1 method=4\n"
        self.assertTrue(any("LABEL:DENY" in f for f in verify_grab.judge(log)))

    def test_no_menu_after_the_escape_fails(self):
        head, _, _ = GOOD.rpartition("SHELL:MENU:OPEN\n")
        self.assertTrue(verify_grab.judge(head))


if __name__ == "__main__":
    unittest.main()
