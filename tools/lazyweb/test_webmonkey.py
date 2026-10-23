"""The browser monkey's reader and actions (`webmonkey_watch.py`, `webmonkey_actions.py`, `webmonkey.py`)."""

import random
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import tempfile
import webmonkey_actions as acts  # noqa: E402
import webmonkey_repro as r  # noqa: E402
import webmonkey_watch as w  # noqa: E402


class WatchTests(unittest.TestCase):
    def test_a_load_that_finishes_is_not_stuck(self):
        watch = w.Watch()
        watch.feed(["LAZYWEB:NAV:https://a.test/"], 0)
        self.assertIsNotNone(watch.stuck_load(100, 90))
        watch.feed(["LAZYWEB:LOAD:https://a.test/"], 10)
        self.assertIsNone(watch.stuck_load(100, 90))
        self.assertEqual((watch.loads, watch.page), (1, "https://a.test/"))

    def test_a_failed_load_is_counted_not_a_finding(self):
        watch = w.Watch()
        self.assertEqual(watch.feed(["LAZYWEB:NAV:http://x/", "LAZYWEB:FAIL:Could not load http://x/"], 0), [])
        self.assertEqual((watch.load_fails, watch.loading_since), (1, None))

    def test_jank_is_a_frame_over_the_limit(self):
        watch = w.Watch(jank_ms=100)
        quiet = "LAZYWEB:INFO:WINDOW:updates=3:full=1:apply=4000:draws=3:draw=1300:max=2900:parts=4"
        slow = "LAZYWEB:INFO:WINDOW:updates=3:full=1:apply=4000:draws=3:draw=1300:max=250000:parts=4"
        self.assertEqual(watch.feed([quiet], 1), [])
        self.assertEqual(len(watch.feed([slow], 2)), 1)
        self.assertEqual(round(watch.worst_frame_ms), 250)

    def test_scroll_jank_is_a_long_gap_between_frames_not_the_draw_cost(self):
        watch = w.Watch(jank_ms=100)
        still = "LAZYWEB:INFO:SMOOTH:wheel=0:frames=22:gapmax=900000:over33=0:shown=9900:checker=0"
        smooth = "LAZYWEB:INFO:SMOOTH:wheel=40:frames=58:gapmax=18000:over33=0:shown=9900:checker=0"
        hitch = "LAZYWEB:INFO:SMOOTH:wheel=140:frames=2:gapmax=194078:over33=1:shown=900:checker=0"
        self.assertEqual(watch.feed([still, smooth], 1), [])  # a gap with no wheel is not scrolling
        self.assertEqual(len(watch.feed([hitch], 2)), 1)
        self.assertEqual((watch.scroll_seconds, round(watch.worst_gap_ms)), (2, 194))

    def test_checkerboard_seconds_are_counted(self):
        watch = w.Watch()
        watch.feed(["LAZYWEB:INFO:SMOOTH:wheel=9:frames=30:gapmax=20000:over33=0:shown=1000:checker=600"], 1)
        self.assertEqual(watch.checker_seconds, 1)

    def test_a_slow_wheel_answer_is_jank_only_with_wheel_input(self):
        watch = w.Watch(answer_ms=400)
        row = "WEBD:NOTE:INFO:PAINT:mode=fast:n=3:needs=0:px=1:clear=1:wk=1:swz=1:post=1:avg=1:max=1:lat=2:wheel=%d:mouse=0:w2d=321139/446524:full=1"
        self.assertEqual(watch.feed([row % 0], 1), [])
        self.assertEqual(len(watch.feed([row % 12], 2)), 1)
        self.assertEqual(round(watch.worst_answer_ms), 447)

    def test_breakage_is_a_finding(self):
        watch = w.Watch()
        for line in ["WEBD:LOST view=3 reason=other", "WEBD:NOTE:WEBPROCESS:UNRESPONSIVE:7", "LAZYWEB:HANG:SHOW",
                     "LazyOS PANIC at x", "SCHED:STUCK tid=4", "LOCK:WAIT lock=ext2 holder=a thread=b ms=900 comm=networkprocess"]:
            self.assertEqual(len(watch.feed([line], 0)), 1, line)
        self.assertEqual(watch.feed(["LOCK:WAIT lock=ext2 holder=a thread=b ms=20 comm=x"], 0), [])


class ActionTests(unittest.TestCase):
    REGION = (100, 80, 800, 500)

    def test_the_same_seed_draws_the_same_actions(self):
        def run(seed):
            rng, pool = random.Random(seed), acts.Pool(["https://a.test/"])
            return [acts.draw(rng, self.REGION, pool) for _ in range(200)]
        self.assertEqual(run(5), run(5))
        self.assertNotEqual(run(5), run(6))

    def test_pointer_targets_are_inside_the_page(self):
        rng, pool = random.Random(1), acts.Pool([])
        x, y, w_, h = self.REGION
        for _ in range(500):
            act = acts.draw(rng, self.REGION, pool)
            for key in ("at", "from", "to"):
                if key in act:
                    self.assertTrue(x <= act[key][0] < x + w_ and y <= act[key][1] < y + h, act)

    def test_every_kind_is_drawn_and_none_closes_the_window(self):
        rng, pool = random.Random(2), acts.Pool(acts.START_URLS)
        kinds = {acts.draw(rng, self.REGION, pool)["a"] for _ in range(3000)}
        self.assertEqual(kinds, set(acts.WEIGHTS))
        self.assertNotIn(("ctrl", "w"), acts.CHROME_KEYS)

    def test_the_pool_walks_to_pages_it_reached(self):
        pool = acts.Pool(["https://seed.test/"])
        pool.note("https://reached.test/a")
        pool.note("about:history")
        self.assertEqual(pool.seen, ["https://reached.test/a"])


class ReproTests(unittest.TestCase):
    RUN = {"commit": "a" * 40, "dirty": False, "branch": "main", "seed": 3, "screen": "1280x720", "memory": "4G",
           "accel": "auto", "start": "arstechnica.com", "duration": 600.0, "jank_ms": 100.0, "answer_ms": 400.0, "build": ""}

    def trail(self, n=60):
        return [{"act": {"a": "scroll", "i": i}, "page": f"https://site.test/{i}", "wait": 0.123456} for i in range(n)]

    def test_findings_have_a_kind_and_a_hang_and_its_lost_view_are_one_bug(self):
        self.assertEqual(r.kind_of("jank: no frame was drawn for 300 ms while scrolling (page x)"), "scroll-gap")
        self.assertEqual(r.kind_of("log: the page's process was reported unresponsive (WEBPROCESS:UNRESPONSIVE)"), "page-hang")
        self.assertEqual(r.key_of("webd lost the window's view (WEBD:LOST view=3)", "https://a.test/x"),
                         r.key_of("the page's process stopped answering: x", "https://a.test/y"))
        self.assertNotEqual(r.key_of("jank: no frame was drawn for 1 ms", "https://a.test/"),
                            r.key_of("jank: no frame was drawn for 1 ms", "https://b.test/"))

    def test_the_replay_loads_the_page_the_first_kept_action_ran_on_and_keeps_the_pauses(self):
        replay = r.steps_from(self.trail(), "")
        self.assertEqual(len(replay["steps"]), r.TAIL)
        self.assertEqual(replay["page"], "https://site.test/20")
        self.assertEqual(replay["steps"][0], {"act": {"a": "scroll", "i": 20}, "wait": 0.12})
        local = [{"act": {"a": "idle"}, "page": "about:history", "wait": 99.0}]
        self.assertEqual(r.steps_from(local, "")["page"], "about:start")
        self.assertEqual(r.steps_from(local, "")["steps"][0]["wait"], r.MAX_WAIT)

    def test_a_replay_is_reproduced_by_a_finding_of_the_same_kind_only(self):
        want = {"kind": "scroll-gap"}
        self.assertEqual(len(r.reproduced(want, ["jank: no frame was drawn for 120 ms while scrolling (page y)"])), 1)
        self.assertEqual(r.reproduced(want, ["jank: the page took 500 ms to answer the wheel (page y)"]), [])

    def test_the_bundle_and_the_issue_name_everything_a_fixer_needs(self):
        b = r.bundle("jank: no frame was drawn for 1190 ms while scrolling (page https://www.theverge.com/)",
                     "https://www.theverge.com/", 88.2, 41, self.trail(), self.RUN, ["SERIAL LINE"])
        self.assertEqual((b["kind"], b["site"], b["measured_ms"]), ("scroll-gap", "https://www.theverge.com", 1190.0))
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp)
            r.write(b, 1, out)
            self.assertEqual(r.load(out / "repro_01.json")["kind"], "scroll-gap")
            text = (out / "issue_01.md").read_text(encoding="utf-8")
        for needle in ("aaaaaaaaaaaa", "--repro", "https://site.test/20", "--seed 3", "--start arstechnica.com",
                       "SERIAL LINE", "INFO:SMOOTH", "1280x720"):
            self.assertIn(needle, text)


if __name__ == "__main__":
    unittest.main()
