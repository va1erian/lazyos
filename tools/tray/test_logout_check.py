#!/usr/bin/env python3
"""The logout judge fails when it should (and passes a good log)."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from logout_check import judge  # noqa: E402

FIRST = """\
LOGIN:LOGOUT:PASS session=1 user=user
INIT:LOGOUT:BEGIN session=1
INIT:APP:QUIT:SENT app=os.lazy.volume grace_ms=3000
INIT:APP:QUIT:SENT app=os.lazy.traydemo grace_ms=3000
INIT:APP:TERM:SENT app=lazyshell grace_ms=3000
VOLUME:QUIT:PASS
TRAYDEMO:QUIT:IGNORED
INIT:LOGOUT:GREETER:HELD
INIT:APP:QUIT:TIMEOUT app=os.lazy.traydemo pid=40
INIT:LOGOUT:PASS session=1 rows=4 tasks=2 ticks=302
LOGIN:GREETER:PASS pid=41
"""

SECOND = """\
LOGIN:LOGOUT:PASS session=2 user=user
INIT:LOGOUT:BEGIN session=2
INIT:APP:QUIT:SENT app=os.lazy.volume grace_ms=3000
INIT:APP:QUIT:SENT app=os.lazy.traydemo grace_ms=3000
VOLUME:QUIT:PASS
TRAYDEMO:QUIT:PASS
INIT:LOGOUT:PASS session=2 rows=4 tasks=1 ticks=40
LOGIN:GREETER:PASS pid=52
"""


class LogoutJudgeTest(unittest.TestCase):
    def test_a_good_log_passes(self):
        self.assertEqual(judge(FIRST + SECOND), [])

    def test_two_logouts_are_needed(self):
        self.assertTrue(judge(FIRST))

    def test_the_bound(self):
        slow = SECOND.replace("ticks=40", "ticks=600")
        self.assertTrue(any("bound" in f for f in judge(FIRST + slow)))

    def test_the_demo_must_quit_before_the_sweep(self):
        late = SECOND.replace("TRAYDEMO:QUIT:PASS\n", "") + "TRAYDEMO:QUIT:PASS\n"
        self.assertTrue(any("before INIT:LOGOUT:PASS" in f for f in judge(FIRST + late)))

    def test_the_ignoring_demo_must_be_killed_at_the_grace(self):
        unkilled = FIRST.replace("INIT:APP:QUIT:TIMEOUT app=os.lazy.traydemo pid=40\n", "")
        self.assertTrue(any("not killed" in f for f in judge(unkilled + SECOND)))

    def test_the_old_sigkill_logout_fails(self):
        old = FIRST.replace("VOLUME:QUIT:PASS\n", "").replace(
            "INIT:APP:QUIT:SENT app=os.lazy.volume grace_ms=3000\n", "")
        failures = judge(old + SECOND)
        self.assertTrue(any("Volume" in f for f in failures))

    def test_the_login_screen_must_wait_for_the_apps(self):
        early = FIRST.replace("LOGIN:GREETER:PASS pid=41\n", "").replace(
            "TRAYDEMO:QUIT:IGNORED\n", "TRAYDEMO:QUIT:IGNORED\nLOGIN:GREETER:PASS pid=41\n")
        self.assertTrue(any("before the apps" in f for f in judge(early + SECOND)))


if __name__ == "__main__":
    unittest.main()
