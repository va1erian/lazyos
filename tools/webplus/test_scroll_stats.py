#!/usr/bin/env python3
"""scroll_stats.py on a small serial log: the window, the answer time, the guard and the main thread's wait."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import scroll_stats  # noqa: E402

PAINT = ("WEBPLUS:INFO:PAINT:mode=fast:n={n}:needs={n}:px=1:clear=0:wk=1:swz=1:post=1:avg=1:max=1:lat=1:wheel={w}:mouse=0:"
         "w2d={a}/{m}:full={f}:delta=1:scroll=1:checks=0:bad=0:wkcalls=1:wkmax=1")


def log() -> list[str]:
    return [
        "PERF:runwait:tick=1000 thread#9=5/900/300 fork#36=100/2000000/900",
        PAINT.format(n=0, w=0, a=0, m=0, f=0),
        PAINT.format(n=10, w=18, a=60000, m=400000, f=2),
        "PERF:CPU tgid=36 pct=50 user=49 kernel=1 class=normal demotions=3 comm=webprocess",
        "PERF:CPU tgid=25 pct=20 user=20 kernel=0 class=normal demotions=0 comm=lazywebplus.elf",
        "SCHED:GUARD:SUMMARY tgid=36 comm=webprocess demotions=12 threads=7 window_s=10",
        "WEBPLUS:INFO:SPEC:shifts=18:snaps=0:snap_px=0:blocked=0:w2s=8000/20000:ahead=120",
        PAINT.format(n=12, w=20, a=80000, m=250000, f=3),
        "PERF:runwait:tick=1400 thread#9=7/1500/300 fork#36=300/4500000/900",
        PAINT.format(n=1, w=0, a=0, m=0, f=0),
    ]


class Stats(unittest.TestCase):
    def test_the_window_is_the_seconds_with_wheel_input_and_the_numbers_are_theirs(self):
        r = scroll_stats.stats(log())
        self.assertEqual(r["seconds"], 2)
        self.assertEqual(r["paints_per_s"], 11.0)
        self.assertEqual(r["wheel_per_s"], 19.0)
        self.assertEqual(r["full_frames"], 5)
        self.assertAlmostEqual(r["w2d_avg_ms"], 70.0)
        self.assertEqual(r["w2d_worst_ms"], 400.0)
        self.assertAlmostEqual(r["w2s_avg_ms"], 8.0)
        self.assertEqual(r["web_cpu_pct"], 50)
        self.assertEqual(r["web_demotions_total"], 12)

    def test_the_main_threads_wait_is_the_difference_of_two_runwait_lines_over_their_span(self):
        r = scroll_stats.stats(log())
        self.assertEqual(r["main_waits"], 200)
        self.assertEqual(r["main_wait_ms"], 2500.0)
        self.assertAlmostEqual(r["window_s"], 4.0)
        self.assertAlmostEqual(r["main_wait_share"], 2500 / 4000)

    def test_a_log_without_wheel_input_says_so_and_without_perf_has_no_wait(self):
        self.assertIn("error", scroll_stats.stats([PAINT.format(n=3, w=0, a=0, m=0, f=0)]))
        plain = [line for line in log() if "runwait" not in line]
        self.assertNotIn("main_wait_ms", scroll_stats.stats(plain))
        self.assertIn("paints/s", scroll_stats.render("x", scroll_stats.stats(plain)))


if __name__ == "__main__":
    unittest.main()