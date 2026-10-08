#!/usr/bin/env python3
"""The window soak's judge fails when it should, and the checked-in session
is what the generator writes."""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import wm_soak  # noqa: E402
from wm_soak import judge  # noqa: E402


def cycle(i: int) -> str:
    return (f"XUID:WM:OPENED id={i} windows=2 title=Calculator\n"
            "CALC:UP:PASS\n"
            f"XUID:WM:CLOSED id={i} windows=1 title=Calculator\n"
            "INIT:LAUNCH:EXIT app=os.lazy.calc status=0\n")


GOOD = "XUID:WM:OPENED id=1 windows=1 title=Terminal\n" + "".join(cycle(i) for i in range(2, 5))


class WmSoakJudgeTest(unittest.TestCase):
    def test_a_good_log_passes(self):
        self.assertEqual(judge(GOOD, 3), [])
        self.assertEqual(judge(GOOD.replace("\n", "\r\n"), 3), [])

    def test_a_missing_cycle(self):
        self.assertTrue(judge(GOOD, 4))

    def test_a_window_left_behind(self):
        leak = GOOD.replace("XUID:WM:CLOSED id=4 windows=1", "XUID:WM:CLOSED id=4 windows=2")
        self.assertTrue(any("left after a close" in f for f in judge(leak, 3)))

    def test_an_app_that_never_exited(self):
        stuck = GOOD.replace("INIT:LAUNCH:EXIT app=os.lazy.calc status=0\n", "", 1)
        self.assertTrue(any("exits" in f for f in judge(stuck, 3)))

    def test_failure_markers(self):
        self.assertTrue(judge(GOOD + "SHELL:LINK:LOST err=32\n", 3))

    def test_the_checked_in_session_is_current(self):
        written = json.loads(wm_soak.SCRIPT.read_text(encoding="utf-8"))
        self.assertEqual(written, json.loads(json.dumps(wm_soak.session(wm_soak.CYCLES))))


if __name__ == "__main__":
    unittest.main()
