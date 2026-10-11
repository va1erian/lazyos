#!/usr/bin/env python3
"""The harness's judges fail when they should, and the mock and the session
are what the guest and the judge expect.

    python tools/lazysky/test_judge.py
"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import judge  # noqa: E402
import live  # noqa: E402
import mockserver  # noqa: E402
import session  # noqa: E402

PASSWORD = "p4ssw0rd-harness"
GOOD_SERIAL = "\n".join([
    "SKY:UP:PASS", "SKY:LOGIN:PASS:me.test", "SKY:FEED:PAGE:30:0", "SKY:FEED:ROWS:30",
    "SKY:IMAGES:1", "SKY:IMAGES:6",
    "SKY:PERF:paint_p50=0.70:paint_p99=1.20:paint_max=2.10:tick_max=1.40:frames=120",
])


def request(path: str, body: str = "", sni: str = "bsky.test", bearer: bool = True) -> dict:
    headers = {"user-agent": "LazySky/0.1.0"}
    if bearer:
        headers["authorization"] = f"Bearer {judge.ACCESS}"
    return {"method": "GET", "path": path, "host": sni, "sni": sni, "headers": headers, "body": body}


def good_requests() -> list[dict]:
    signin = request("/xrpc/com.atproto.server.createSession",
                     json.dumps({"identifier": "me.test", "password": PASSWORD}), bearer=False)
    signin["method"] = "POST"
    return [signin, request("/xrpc/app.bsky.feed.getTimeline?limit=30"),
            *(request(f"/thumb/{n}.jpg", sni="cdn.bsky.test", bearer=False) for n in range(4))]


class SerialTests(unittest.TestCase):
    def test_a_good_run_passes(self) -> None:
        self.assertEqual(judge.judge_serial(GOOD_SERIAL, PASSWORD), [])

    def test_a_missing_marker_is_named(self) -> None:
        text = GOOD_SERIAL.replace("SKY:LOGIN:PASS:me.test\n", "")
        problems = judge.judge_serial(text, PASSWORD)
        self.assertTrue(any("SKY:LOGIN:PASS:me.test" in p for p in problems), problems)

    def test_too_few_pictures_fail(self) -> None:
        text = GOOD_SERIAL.replace("SKY:IMAGES:6", "SKY:IMAGES:2")
        self.assertTrue(any("pictures" in p for p in judge.judge_serial(text, PASSWORD)))

    def test_a_failure_marker_fails(self) -> None:
        text = GOOD_SERIAL + "\nSKY:RUN:FAIL:the window could not be built"
        self.assertTrue(any("failure marker" in p for p in judge.judge_serial(text, PASSWORD)))

    def test_a_leaked_secret_fails(self) -> None:
        for secret in (PASSWORD, judge.ACCESS, judge.REFRESH):
            problems = judge.judge_serial(GOOD_SERIAL + f"\ndebug {secret}", PASSWORD)
            self.assertTrue(any("serial log" in p for p in problems), secret)


class PerfTests(unittest.TestCase):
    def test_within_budget_passes_on_both_machines(self) -> None:
        self.assertEqual(judge.judge_perf(GOOD_SERIAL, True), [])
        self.assertEqual(judge.judge_perf(GOOD_SERIAL, False), [])

    def test_no_report_fails(self) -> None:
        self.assertTrue(judge.judge_perf("SKY:UP:PASS", True))

    def test_each_budget_breach_is_reported(self) -> None:
        slow = GOOD_SERIAL.replace("paint_p99=1.20", "paint_p99=9.50")
        self.assertTrue(any("p99" in p for p in judge.judge_perf(slow, True)))
        self.assertEqual(judge.judge_perf(slow, False), [], "TCG judges the max, not the p99")
        spike = GOOD_SERIAL.replace("paint_max=2.10", "paint_max=40.0")
        self.assertTrue(any("max" in p for p in judge.judge_perf(spike, False)))
        busy = GOOD_SERIAL.replace("tick_max=1.40", "tick_max=7.00")
        self.assertTrue(any("tick" in p for p in judge.judge_perf(busy, True)))


class RequestTests(unittest.TestCase):
    def test_what_a_good_run_sends_passes(self) -> None:
        self.assertEqual(judge.judge_requests(good_requests(), PASSWORD), [])

    def test_no_sign_in_fails(self) -> None:
        self.assertTrue(judge.judge_requests(good_requests()[1:], PASSWORD))

    def test_a_wrong_password_or_handle_fails(self) -> None:
        reqs = good_requests()
        reqs[0]["body"] = json.dumps({"identifier": "someone.else", "password": PASSWORD})
        self.assertTrue(any("identifier" in p for p in judge.judge_requests(reqs, PASSWORD)))
        reqs = good_requests()
        reqs[0]["body"] = json.dumps({"identifier": "me.test", "password": "other"})
        self.assertTrue(any("password" in p for p in judge.judge_requests(reqs, PASSWORD)))

    def test_a_call_without_the_token_fails(self) -> None:
        reqs = good_requests() + [request("/xrpc/app.bsky.feed.getTimeline", bearer=False)]
        self.assertTrue(any("bearer" in p for p in judge.judge_requests(reqs, PASSWORD)))

    def test_the_wrong_sni_fails(self) -> None:
        reqs = good_requests()
        reqs[2]["sni"] = "bsky.test"
        self.assertTrue(any("SNI" in p for p in judge.judge_requests(reqs, PASSWORD)))

    def test_too_few_pictures_fail(self) -> None:
        reqs = good_requests()[:4]
        self.assertTrue(any("picture" in p for p in judge.judge_requests(reqs, PASSWORD)))


class LiveTests(unittest.TestCase):
    GOOD = "\n".join(["SKY:UP:PASS", "SKY:FEED:ROWS:30", "SKY:IMAGES:5"])

    def test_a_quiet_run_passes(self) -> None:
        self.assertEqual(live.judge_live(self.GOOD, "pw", "me.test"), [])

    def test_a_restart_is_named(self) -> None:
        problems = live.judge_live(self.GOOD + "\nSKY:UP:PASS", "pw", "me.test")
        self.assertTrue(any("restarted" in p for p in problems), problems)

    def test_a_panic_and_a_leak_fail(self) -> None:
        text = self.GOOD + "\nSKY:PANIC:index out of bounds at x.rs:1\npw"
        problems = live.judge_live(text, "pw", "me.test")
        self.assertTrue(any("panic" in p for p in problems))
        self.assertTrue(any("password" in p for p in problems))

    def test_the_script_holds_no_secret(self) -> None:
        text = json.dumps(live.steps(2, 10))
        self.assertIn('"type_secret": "BSKY_DUMMY_PWD"', text)
        self.assertIn('"type_secret": "BSKY_DUMMY_ACCOUNT"', text)


class FixtureTests(unittest.TestCase):
    def test_posts_are_deterministic_and_varied(self) -> None:
        self.assertEqual(mockserver.post_view(7), mockserver.post_view(7))
        views = [mockserver.post_view(n) for n in range(mockserver.POSTS)]
        self.assertTrue(any("embed" in v for v in views))
        kinds = {v["embed"]["$type"] for v in views if "embed" in v}
        self.assertEqual(kinds, {"app.bsky.embed.images#view", "app.bsky.embed.record#view"})
        self.assertTrue(any(v["author"]["did"] == mockserver.ME for v in views))

    def test_pages_chain_to_the_end(self) -> None:
        pages = (mockserver.POSTS + mockserver.PAGE - 1) // mockserver.PAGE
        self.assertIn("cursor", mockserver.timeline_page(0))
        self.assertNotIn("cursor", mockserver.timeline_page(pages - 1))
        self.assertEqual(len(mockserver.timeline_page(0)["feed"]), mockserver.PAGE)

    def test_the_big_picture_is_a_real_large_png(self) -> None:
        data = mockserver.picture("full", "big")
        self.assertTrue(data.startswith(b"\x89PNG"))
        width = int.from_bytes(data[16:20], "big")
        height = int.from_bytes(data[20:24], "big")
        self.assertEqual((width, height), mockserver.BIG)

    def test_a_thumbnail_is_a_jpeg(self) -> None:
        self.assertTrue(mockserver.picture("thumb", "5-0").startswith(b"\xff\xd8"))


class SessionTests(unittest.TestCase):
    def test_the_checked_in_example_is_current(self) -> None:
        self.assertEqual(session.EXAMPLE.read_text(encoding="utf-8"), session.render(session.steps()))

    def test_no_secret_is_written_into_the_script(self) -> None:
        text = session.render(session.steps())
        self.assertIn('"type_secret": "LAZYSKY_PASSWORD"', text)
        self.assertNotIn("hunter", text)

    def test_the_session_waits_for_the_markers_the_judge_needs(self) -> None:
        gates = [s["wait_for"] for s in session.steps() if "wait_for" in s]
        for marker in ("SKY:UP:PASS", "SKY:LOGIN:PASS:me.test", "SKY:FEED:ROWS:30"):
            self.assertIn(marker, gates)

    def test_the_app_is_launched_only_after_the_settings_are_confirmed(self) -> None:
        steps = session.steps()
        gate = next(i for i, s in enumerate(steps) if s.get("wait_for") == "SKYH:settings:1")
        launch = next(i for i, s in enumerate(steps)
                      if "rhai /tmp/l.rhai" in str(s.get("type", "")))
        secret = next(i for i, s in enumerate(steps) if "type_secret" in s)
        self.assertLess(gate, launch)
        self.assertLess(launch, secret)


if __name__ == "__main__":
    unittest.main()
