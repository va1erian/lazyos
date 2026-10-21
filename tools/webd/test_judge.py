#!/usr/bin/env python3
"""The webd harness judge fails when it should (`judge.py`, `run.py --judge-only`, `session.py`,
`listener.py`): canned logs, one failure at a time."""

from __future__ import annotations

import json
import re
import socket
import struct
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.error
import urllib.request
import zlib
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(HERE))
import judge as j  # noqa: E402
import listener  # noqa: E402
import session  # noqa: E402

CLEAN = {"requests": [], "connections": 0}


def good(loss: bool = True) -> str:
    """The log of a good run: webd up, one view and its page, a click, the kill, a new view, the quit."""
    lines = [
        "WEBD:UP:PASS",
        "WEBD:SERVE:app.os.lazy.webd.light",
        "WEBDDEMO:UP:PASS",
        "UI:RECT x=100 y=80 w=480 h=320 name=window:Web View Demo",
        "WEBD:VIEW:CREATE client=app:os.lazy.webddemo view=1",
        "WEBD:PROCESS:LAUNCH:web:31",
        "WEBDDEMO:VIEW:PASS id=1",
        "WEBDDEMO:STATE:Web View Demo",
        "WEBDDEMO:FRAME:PASS colour=blue w=480 h=320 seq=2 ms=812",
        "WEBDDEMO:LATENCY:812",
        "UI:WIDGET x=20 y=70 w=260 h=40 name=link window=Web View Demo",
        f"WEBDDEMO:LINK:{j.LINK_URL}",
        f"WEBDDEMO:LINK:{j.LINK_URL}?q=okhere",
    ]
    if loss:
        lines += [
            "TERM:CMD:kill -9 31",
            "WEBD:LOST view=1 reason=crashed",
            "WEBDDEMO:LOST:crashed",
            "WEBD:VIEW:CREATE client=app:os.lazy.webddemo view=2",
            "WEBDDEMO:VIEW:PASS id=2",
            "WEBD:VIEW:DESTROY view=1",
            "WEBDDEMO:RECREATE:PASS",
        ]
    lines += ["WEBDDEMO:QUIT:PASS", f"WEBD:VIEW:DESTROY view={2 if loss else 1}"]
    return "\r\n".join(lines) + "\r\n"


def png(path: Path, width: int, height: int, paint) -> None:
    """A truecolour PNG whose pixel (x, y) is `paint(x, y)`."""
    rows = b"".join(b"\0" + b"".join(bytes(paint(x, y)) for x in range(width)) for y in range(height))

    def chunk(kind: bytes, body: bytes) -> bytes:
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
                     + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def window_painter(colour):
    """A 640x480 screen with the demo window (100,80 480x320) in `colour` and grey around it."""
    return lambda x, y: colour if 100 <= x < 580 and 80 <= y < 400 else (90, 90, 90)


BLUE, WHITE = (10, 10, 255), (255, 255, 255)


class Judge(unittest.TestCase):
    def assert_fails(self, problems: list[str], *needles: str) -> None:
        self.assertTrue(problems, "the judge passed what it should have failed")
        for needle in needles:
            self.assertTrue(any(needle in p for p in problems), f"{needle!r} not in {problems}")

    def test_a_good_run_passes_every_judge(self):
        text = good()
        for judge in (j.judge_webd, j.judge_views, j.judge_page, j.judge_link, j.judge_quit, j.judge_failures,
                      j.judge_denies):
            self.assertEqual(judge(text), [], judge.__name__)
        self.assertEqual(j.judge_loss(text, True), [])
        self.assertEqual(j.judge_requests(CLEAN), [])
        skipped = good(loss=False)
        self.assertEqual(j.judge_loss(skipped, False), [])
        self.assertEqual(j.judge_quit(skipped), [])

    def test_webd_must_be_up_and_serving(self):
        self.assert_fails(j.judge_webd(good().replace("WEBD:UP:PASS", "WEBD:UP:WAIT")), "WEBD:UP:PASS")
        self.assert_fails(j.judge_webd(good().replace("WEBD:SERVE:app.os.lazy.webd.light", "")), "SERVE")

    def test_a_view_the_demo_has_must_have_been_made_by_webd_for_the_demo(self):
        no_views = re.sub(r"WEBDDEMO:VIEW:PASS[^\r]*\r\n", "", good())
        self.assert_fails(j.judge_views(no_views), "never printed")
        self.assert_fails(j.judge_views(good().replace("view=1\r\n", "view=7\r\n", 1)), "no WEBD:VIEW:CREATE")
        self.assert_fails(j.judge_views(good().replace("app:os.lazy.webddemo view=1", "app:os.lazy.other view=1")),
                          "not for the demo")
        self.assert_fails(j.judge_views(good().replace("id=2", "id=1")), "twice")

    def test_the_first_frame_must_be_the_blue_page(self):
        self.assert_fails(j.judge_page(good().replace("colour=blue", "colour=white")), "not blue")
        self.assert_fails(j.judge_page(re.sub(r"WEBDDEMO:FRAME:PASS[^\r]*\r\n", "", good())), "FRAME:PASS")
        self.assert_fails(j.judge_page(good().replace("w=480 h=320", "w=1 h=1")), "1x1")
        self.assert_fails(j.judge_page(good().replace("WEBDDEMO:LATENCY:812\r\n", "")), "LATENCY")
        self.assert_fails(j.judge_page(good().replace("LATENCY:812", "LATENCY:900")), "agree")
        slow = good().replace("ms=812", "ms=90000").replace("LATENCY:812", "LATENCY:90000")
        self.assert_fails(j.judge_page(slow), "took 90000")
        self.assert_fails(j.judge_page(good().replace("STATE:Web View Demo", "STATE:Blank")), "STATE")
        late = good().replace("WEBDDEMO:VIEW:PASS id=1\r\n", "")
        late = late.replace("WEBDDEMO:LATENCY:812\r\n", "WEBDDEMO:LATENCY:812\r\nWEBDDEMO:VIEW:PASS id=1\r\n")
        self.assert_fails(j.judge_page(late), "before the view")

    def test_the_click_must_come_back_as_the_link_and_nothing_else_may(self):
        form_only = good().replace(f"WEBDDEMO:LINK:{j.LINK_URL}\r\n", "", 1)
        self.assert_fails(j.judge_link(form_only), "after the click")  # the form's submit is not the click
        none = re.sub(r"WEBDDEMO:LINK:[^\r]*\r\n", "", good())
        self.assert_fails(j.judge_link(none), "after the click")
        other = good() + "WEBDDEMO:LINK:http://evil.example/\r\n"
        self.assert_fails(j.judge_link(other), "unexpected link")

    def test_the_listener_must_have_seen_nothing(self):
        saw = {"requests": [{"method": "GET", "path": "/blocked.png", "client": "127.0.0.1"}], "connections": 1}
        self.assert_fails(j.judge_requests(saw), "GET /blocked.png")
        silent = {"requests": [], "connections": 2}
        self.assert_fails(j.judge_requests(silent), "connected and sent nothing")

    def test_the_loss_must_be_reported_and_healed(self):
        text = good()
        self.assert_fails(j.judge_loss(text.replace("WEBDDEMO:LOST:crashed\r\n", ""), True), "never printed WEBDDEMO:LOST")
        self.assert_fails(j.judge_loss(text.replace("LOST:crashed", "LOST:service_gone"), True), "service_gone")
        self.assert_fails(j.judge_loss(text.replace("WEBDDEMO:RECREATE:PASS\r\n", ""), True), "RECREATE")
        self.assert_fails(j.judge_loss(text.replace("WEBDDEMO:VIEW:PASS id=2\r\n", ""), True), "second")
        self.assert_fails(j.judge_loss(text.replace("id=2", "id=1"), True), "second")
        self.assert_fails(j.judge_loss(text.replace("WEBD:LOST view=1 reason=crashed\r\n", ""), True), "WEBD:LOST view=1")
        self.assert_fails(j.judge_loss(text.replace("WEBD:VIEW:DESTROY view=1\r\n", ""), True), "never destroyed")
        swapped = text.replace("WEBDDEMO:RECREATE:PASS\r\n", "").replace("WEBDDEMO:LOST:crashed",
                                                                         "WEBDDEMO:RECREATE:PASS\r\nWEBDDEMO:LOST:crashed")
        self.assert_fails(j.judge_loss(swapped, True), "before LOST")
        # A loss nobody asked for is a failure too.
        self.assert_fails(j.judge_loss(text, False), "without the session asking")

    def test_the_demo_must_quit_and_webd_must_let_go(self):
        self.assert_fails(j.judge_quit(good().replace("WEBDDEMO:QUIT:PASS", "")), "QUIT")
        self.assert_fails(j.judge_quit(good().replace("WEBD:VIEW:DESTROY view=2\r\n", "")), "[2]")

    def test_refused_calls_are_judged_against_the_documented_set(self):
        own = "LABEL:DENY label=app:os.lazy.webddemo iface=0x1 method=2"
        self.assert_fails(j.judge_denies(good() + own + "\r\n"), "webddemo")
        webd = "LABEL:DENY label=app:os.lazy.webd iface=0x1 method=2"
        self.assert_fails(j.judge_denies(good() + webd + "\r\n"), "app:os.lazy.webd ")
        optional = "LABEL:DENY label=app:os.lazy.webd/webprocess file=/etc/localtime access=stat"
        self.assertEqual(j.judge_denies(good() + optional + "\r\n"), [])
        other = "LABEL:DENY label=app:os.lazy.webd/webprocess file=/home/user/secret access=read"
        self.assert_fails(j.judge_denies(good() + other + "\r\n"), "secret")
        # The optional set is for children only: the app itself gets none.
        same_text = "LABEL:DENY label=app:os.lazy.webd file=/etc/localtime access=stat"
        self.assert_fails(j.judge_denies(good() + same_text + "\r\n"), "localtime")
        stranger = "LABEL:DENY label=app:os.lazy.lazysky iface=0x1 method=2"
        self.assertEqual(j.judge_denies(good() + stranger + "\r\n"), [])

    def test_a_fail_marker_fails_the_run(self):
        self.assert_fails(j.judge_failures(good() + "WEBDDEMO:CREATE:FAIL:refused (errno 28)\r\n"), "CREATE:FAIL")
        self.assert_fails(j.judge_failures(good() + "WEBD:VIEW:FAIL:engine\r\n"), "WEBD:VIEW:FAIL")
        self.assert_fails(j.judge_failures(good() + "WEBDDEMO:RUN:FAIL:window\r\n"), "RUN:FAIL")
        self.assertEqual(j.judge_failures(good()), [])

    def test_the_window_region_and_the_pixels(self):
        text = good() + "UI:RECT x=1 y=2 w=3 h=4 name=window:Terminal\r\n"
        self.assertEqual(j.window_region(text), (100, 80, 480, 320))
        self.assertIsNone(j.window_region("nothing"))
        with tempfile.TemporaryDirectory() as tmp:
            blue, white = Path(tmp) / "blue.png", Path(tmp) / "white.png"
            png(blue, 640, 480, window_painter(BLUE))
            png(white, 640, 480, window_painter(WHITE))
            region = (100, 80, 480, 320)
            self.assertEqual(j.judge_shot(blue, region, "page"), [])
            self.assert_fails(j.judge_shot(white, region, "page"), "only 0%")
            self.assert_fails(j.judge_shot(Path(tmp) / "nope.png", region, "page"), "missing")
            self.assert_fails(j.judge_shot(blue, None, "page"), "UI_PROBE")
            self.assert_fails(j.judge_shot(blue, (1000, 1000, 5, 5), "page"), "outside")
            # Half covered by another window: the share falls under the line.
            half = lambda x, y: BLUE if 100 <= x < 300 and 80 <= y < 400 else (90, 90, 90)  # noqa: E731
            covered = Path(tmp) / "covered.png"
            png(covered, 640, 480, half)
            self.assert_fails(j.judge_shot(covered, region, "page"), "%")


class Constants(unittest.TestCase):
    """The judge's copies of the page's constants are the page's."""

    def test_they_are_the_pages(self):
        page = (ROOT / "xui-app" / "crates" / "webd-client" / "src" / "page.rs").read_text(encoding="utf-8")
        self.assertIn(f'pub const LINK_URL: &str = "{j.LINK_URL}";', page)
        self.assertIn(f'pub const TITLE: &str = "{j.WINDOW}";', page)
        self.assertIn(f"10.0.2.2:{j.BLOCKED_PORT}{j.BLOCKED_PATH}", page)
        self.assertEqual(listener.PORT, j.BLOCKED_PORT)

    def test_the_markers_are_the_demos(self):
        demo = (ROOT / "xui-app" / "crates" / "webd-client" / "src" / "demo.rs").read_text(encoding="utf-8")
        for text in ("VIEW:PASS id=", "FRAME:PASS colour=", "LATENCY:", "LINK:", "STATE:", "LOST:",
                     "RECREATE:PASS"):
            self.assertIn(text, demo)
        channel = (ROOT / "libs" / "tracefmt" / "src" / "channel.rs").read_text(encoding="utf-8")
        self.assertIn(f'Webddemo => "{j.D}"', channel)

    def test_the_session_examples_are_current(self):
        for path, text in session.examples().items():
            self.assertTrue(path.is_file(), f"{path} is missing: python tools/webd/session.py --write")
            self.assertEqual(path.read_text(encoding="utf-8"), text, f"{path} is stale")

    def test_the_session_has_the_steps_the_judge_needs(self):
        steps = session.light()
        waits = [s["wait_for"] for s in steps if "wait_for" in s]
        for marker in ("WEBD:UP:PASS", "WEBDDEMO:UP:PASS", "WEBDDEMO:FRAME:PASS colour=blue",
                       "WEBDDEMO:LOST:", "WEBDDEMO:RECREATE:PASS"):
            self.assertIn(marker, waits)
        shots = [s["shot"] for s in steps if "shot" in s]
        self.assertEqual(shots, ["w01_page", "w02_link", "w03_form", "w04_recreated"])
        skipped = session.light(skip_loss=True)
        self.assertNotIn("w04_recreated", [s.get("shot") for s in skipped])
        self.assertFalse(any("kill" in str(s) for s in skipped))
        self.assertTrue(all(s["timeout"] > 0 for s in steps if "wait_for" in s))


class Listener(unittest.TestCase):
    def test_it_counts_requests_and_bare_connections(self):
        port = _free_port()
        seen = listener.Listener(port=port)
        try:
            self.assertTrue(listener.busy(port))
            with self.assertRaises(urllib.error.HTTPError) as caught:
                urllib.request.urlopen(f"http://127.0.0.1:{port}/blocked.png", timeout=5)
            caught.exception.close()  # a 404, as the listener answers everything
            with socket.create_connection(("127.0.0.1", port), timeout=5):
                pass  # connects and says nothing
            deadline = 50
            while seen.snapshot()["connections"] < 3 and deadline:
                deadline -= 1
                time.sleep(0.1)
            record = seen.snapshot()
        finally:
            seen.close()
        self.assertEqual([r["path"] for r in record["requests"]], ["/blocked.png"])
        # Three: the request, the bare one, and `busy`'s own probe above.
        self.assertEqual(record["connections"], 3)
        self.assertEqual(len(j.judge_requests(record)), 2)
        self.assertFalse(listener.busy(port))


def _free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


class JudgeOnly(unittest.TestCase):
    """`run.py --judge-only DIR` on a canned run directory, as the integrator will use it."""

    def run_only(self, directory: Path, *flags: str) -> subprocess.CompletedProcess:
        return subprocess.run([sys.executable, str(HERE / "run.py"), "--judge-only", str(directory), *flags],
                              capture_output=True, text=True)

    def canned(self, tmp: str, text: str, record: dict, colour=BLUE) -> Path:
        directory = Path(tmp)
        (directory / "serial.log").write_text(text, encoding="utf-8")
        (directory / "requests.json").write_text(json.dumps(record), encoding="utf-8")
        for name in ("w01_page", "w04_recreated"):
            png(directory / f"shot_{name}.png", 640, 480, window_painter(colour))
        return directory

    def test_a_good_run_passes_and_a_leak_or_a_blank_window_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            done = self.run_only(self.canned(tmp, good(), CLEAN))
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            self.assertIn("WEBD:HARNESS:PASS", done.stdout)
        with tempfile.TemporaryDirectory() as tmp:
            leak = {"requests": [{"method": "GET", "path": "/blocked.png", "client": "10.0.2.15"}], "connections": 1}
            done = self.run_only(self.canned(tmp, good(), leak))
            self.assertEqual(done.returncode, 1)
            self.assertIn("WEBD:NETWORK:FAIL", done.stdout)
            self.assertIn("WEBD:HARNESS:FAIL", done.stdout)
        with tempfile.TemporaryDirectory() as tmp:
            done = self.run_only(self.canned(tmp, good(), CLEAN, colour=WHITE))
            self.assertEqual(done.returncode, 1)
            self.assertIn("WEBD:SHOT:FAIL", done.stdout)

    def test_skipping_the_loss(self):
        with tempfile.TemporaryDirectory() as tmp:
            done = self.run_only(self.canned(tmp, good(loss=False), CLEAN), "--skip-loss")
            self.assertEqual(done.returncode, 0, done.stdout + done.stderr)
            self.assertIn("WEBD:LOSS:PASS", done.stdout)


if __name__ == "__main__":
    unittest.main()
