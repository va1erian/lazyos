#!/usr/bin/env python3
"""frame_run.py's reader of a serial log, and its session, on made-up input."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import frame_run  # noqa: E402

LOG = """\
WEBPLUS:LOAD:https://theoldnet.com/webplus/scroll.html
WEBPLUS:TITLE:WebPlus scroll
WEBPROCESS:UP:39
NETWORKPROCESS:UP:41
WEBPLUS:INFO:PAINT:mode=fast:n=40:needs=50:px=36000000:clear=0:wk=40000:swz=8000:post=20000:avg=1700:max=3000:lat=9000:wheel=30:mouse=0:w2d=12000/30000
WEBPLUS:INFO:PAINT:mode=fast:n=20:needs=20:px=18000000:clear=0:wk=20000:swz=4000:post=10000:avg=1700:max=2500:lat=9000:wheel=15:mouse=0:w2d=14000/20000
PERF:CPU tgid=39 pct=62 user=60 kernel=2 class=normal demotions=3 comm=fork
PERF:CPU tgid=30 pct=21 user=9 kernel=12 class=interactive demotions=0 comm=xuid
PERF:CPU tgid=39 pct=58 user=56 kernel=2 class=normal demotions=3 comm=fork
"""


class Summary(unittest.TestCase):
    def test_roles_and_averages(self):
        table, enough = frame_run.summarise(LOG)
        self.assertTrue(enough)
        # The engine process is named from its UP marker, not from `fork`.
        self.assertRegex(table, r"WebProcess\s+2\s+60\s+58\s+2")
        self.assertRegex(table, r"xuid\s+1\s+21\s+9\s+12")
        self.assertNotIn("fork", table)
        self.assertIn("60 paints", table)
        # 60 + 21 + 58 / 2 looks: WebProcess 60, xuid 21 -> 81% busy over 60 paints in 2 s = 30 a second.
        self.assertIn("CPU per painted frame: 27.0 ms", table)
        self.assertIn("0.90 Mpx a paint", table)
        self.assertRegex(table, r"wk\s+1000 us a paint")
        self.assertIn("worst", table)

    def test_kernel_present_metric_is_shown_when_the_image_has_it(self):
        line = "PERF:present_mpx:n=12 p50_us=1500.0 p90_us=1900.5 p99_us=2400.0 max_us=3100.0 mean_us=1600.0\n"
        table, _ = frame_run.summarise(LOG + line)
        self.assertIn("n=12 p50=1500.0 p90=1900.5 p99=2400.0 max=3100.0", table)
        self.assertNotIn("present_mpx", frame_run.summarise(LOG)[0])

    def test_window_thread_and_delta_counts_are_shown(self):
        extra = ("WEBPLUS:INFO:WINDOW:updates=20:full=2:apply=4000:draws=22:draw=44000:max=9000:parts=7\n"
                 "WEBPLUS:INFO:PAINT:mode=fast:n=10:needs=10:px=900000:clear=0:wk=5000:swz=900:post=1000:"
                 "avg=700:max=900:lat=1:wheel=5:mouse=0:w2d=1000/2000:full=2:delta=8:scroll=7\n")
        table, _ = frame_run.summarise(LOG + extra)
        self.assertIn("20 updates, 2 whole", table)
        self.assertIn("apply 200 us an update", table)
        self.assertIn("draw 2000 us a draw", table)
        self.assertIn("up to 7 parts", table)
        self.assertIn("2 whole frames, 8 deltas, 7 of them scrolls", table)

    def test_a_picture_that_differed_from_a_whole_paint_fails_the_run(self):
        good = LOG + "WEBPLUS:INFO:PAINT:mode=fast:n=3:needs=3:px=9:clear=0:wk=1:swz=1:post=1:avg=1:max=1:lat=1:wheel=1:mouse=0:w2d=1/1:full=1:delta=2:scroll=2:checks=2:bad=0\n"
        table, ok = frame_run.summarise(good)
        self.assertTrue(ok)
        self.assertIn("2 checks, 0 differed", table)
        table, ok = frame_run.summarise(good + "WEBPLUS:FAIL:PAINT:MISMATCH:pixels=3:first=1,2:view=10x10\n")
        self.assertFalse(ok)
        self.assertIn("differed from a whole paint", table)

    def test_nothing_busy_is_not_enough(self):
        table, enough = frame_run.summarise("WEBPLUS:TITLE:WebPlus scroll\n")
        self.assertFalse(enough)
        self.assertIn("no PERF:CPU", table)
        self.assertIn("no INFO:PAINT", table)

    def test_lines_before_the_page_loaded_are_ignored(self):
        early = "PERF:CPU tgid=7 pct=90 user=90 kernel=0 class=normal demotions=0 comm=boot\n"
        table, _ = frame_run.summarise(early + LOG)
        self.assertNotIn("boot", table)


class Session(unittest.TestCase):
    def test_wheel_turns_both_ways_and_ends_with_a_shot(self):
        steps = frame_run.frame_session(20.0, 10.0)
        scrolls = [s["mouse_scroll"] for s in steps if "mouse_scroll" in s]
        self.assertTrue(any(v < 0 for v in scrolls) and any(v > 0 for v in scrolls))
        self.assertEqual(steps.index({"shot": "scrolled"}) > steps.index({"shot": "page"}), True)
        self.assertGreaterEqual(len(scrolls) * (frame_run.PAUSE + frame_run.STEP_COST), 19.0)
        self.assertEqual(sum(scrolls[:8]), -3 * 8)

    def test_a_run_never_outlasts_the_page(self):
        steps = [s["mouse_scroll"] for s in frame_run.scroll_steps(60.0) if "mouse_scroll" in s]
        position = longest = 0
        for step in steps:
            position += step
            longest = max(longest, -position)
        self.assertLessEqual(longest, frame_run.RUN_NOTCHES)

    def test_the_scroll_page_is_current(self):
        import scroll_page
        self.assertTrue(scroll_page.PAGE.is_file() and scroll_page.PAGE.read_bytes() == scroll_page.render().encode("utf-8"))


if __name__ == "__main__":
    unittest.main()
