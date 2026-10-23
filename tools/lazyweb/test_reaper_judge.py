#!/usr/bin/env python3
"""The reaper judge (`reaper_judge.py`, issue #887) passes a launcher whose reapers sleep and fails a polling one.

Run: python tools/lazyweb/test_reaper_judge.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import reaper_judge  # noqa: E402

P = reaper_judge.P
LAUNCHES = f"{P}:PROCESS:LAUNCH:web:30\n{P}:PROCESS:LAUNCH:network:32\n"


def log(*counts: int, launches: str = LAUNCHES) -> str:
    return launches + "".join(f"{P}:INFO:REAPER:wakeups={n}\n" for n in counts)


class ReaperJudgeTests(unittest.TestCase):
    def test_sleeping_reapers_pass(self) -> None:
        # Nothing woke at 10 s and at 30 s; the two children ended at the close.
        self.assertEqual(reaper_judge.judge_reaper(log(0, 0, 2)), [])
        self.assertEqual(reaper_judge.judge_reaper(log(0, 0)), [])

    def test_polling_reapers_fail(self) -> None:
        # 20 Hz per process: 400 wakeups in the first 10 s, 1200 by 30 s.
        problems = reaper_judge.judge_reaper(log(400, 1200))
        self.assertEqual(len(problems), 1)
        self.assertIn("polls", problems[0])

    def test_the_last_line_is_the_one_judged(self) -> None:
        self.assertEqual(len(reaper_judge.judge_reaper(log(0, 0, 900))), 1)

    def test_a_missing_counter_fails(self) -> None:
        self.assertTrue(reaper_judge.judge_reaper(LAUNCHES))
        self.assertTrue(reaper_judge.judge_reaper(""))

    def test_no_launch_fails(self) -> None:
        self.assertTrue(reaper_judge.judge_reaper(log(0, launches="")))

    def test_more_launches_allow_more_wakeups(self) -> None:
        relaunched = LAUNCHES + f"{P}:PROCESS:LAUNCH:web:31\n{P}:PROCESS:LAUNCH:web:33\n"
        self.assertTrue(reaper_judge.judge_reaper(log(20)))
        self.assertEqual(reaper_judge.judge_reaper(log(20, launches=relaunched)), [])


if __name__ == "__main__":
    unittest.main()
