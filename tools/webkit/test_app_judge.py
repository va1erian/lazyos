#!/usr/bin/env python3
"""The W5 browser harness (``appjudge.py``, ``app_session.py``, ``app_run.py``) passes a good run
and fails each way it can go wrong; the checked-in session is current.

Run: python tools/webkit/test_app_judge.py
"""

from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import app_run  # noqa: E402
import app_session  # noqa: E402
import appjudge as aj  # noqa: E402
import imgenc  # noqa: E402  (tools/web, put on the path by appjudge)
import sites  # noqa: E402
from sites import Record, Request  # noqa: E402

GOOD = f"""boot noise
WEBKIT:W5:UP
WEBKIT:W5:UI:UP
WEBKIT:W5:UI:FRAME:1000x498
WEBKIT:W5:INFO:RESIZE:1000x498
WEBKIT:W5:INFO:RESIZE:1272x640
WEBKIT:W5:WEBPROCESS:UP:12
WEBKIT:W5:NETWORKPROCESS:UP:13
WEBKIT:W5:NAV:http://example.com/
WEBKIT:W5:LOAD:http://example.com/
WEBKIT:W5:TITLE:{aj.EXAMPLE_TITLE}
WEBKIT:W5:JS:{aj.EXAMPLE_TITLE}
WEBKIT:W5:INFO:TIME:firstpaint=4100:sincecommit=300:sincenav=1900
WEBKIT:W5:INFO:TIME:nav=1:start=2200:commit=3800:finish=4200:load=2000
WEBKIT:W5:LOAD:{aj.OLDNET_URL}
WEBKIT:W5:TITLE:{aj.OLDNET_TITLE}
WEBKIT:W5:JS:{aj.OLDNET_TITLE}
WEBKIT:W5:LOAD:{aj.TALL_URL}
WEBKIT:W5:TITLE:{aj.TALL_TITLE}
WEBKIT:W5:LOAD:{aj.JS_URL}
WEBKIT:W5:TITLE:js-off
WEBKIT:W5:TITLE:{aj.JS_TITLE}
WEBKIT:W5:JS:{aj.JS_TITLE}
WEBKIT:W5:LOAD:{aj.LINKS_URL}
WEBKIT:W5:TITLE:{aj.LINKS_TITLE}
WEBKIT:W5:JS:{aj.LINKS_TITLE}
WEBKIT:W5:CLICK:{aj.FORM_URL}
WEBKIT:W5:LOAD:{aj.FORM_URL}
WEBKIT:W5:TITLE:{aj.FORM_TITLE}
WEBKIT:W5:JS:{aj.FORM_TITLE}
WEBKIT:W5:LOAD:https://theoldnet.com/webkit/echo?q=hello+lazyos
WEBKIT:W5:TITLE:{aj.ECHO_TITLE}
WEBKIT:W5:INPUT:{aj.ECHO_TITLE}
WEBKIT:W5:NAV:{aj.FAIL_URL}
WEBKIT:W5:FAIL:provisional:{aj.FAIL_URL}:Could not connect to server (code 300)
WEBKIT:W5:UI:ERROR:Could not load {aj.FAIL_URL}: Could not connect to server
WEBKIT:W5:START:{aj.SLOW_URL}
WEBKIT:W5:CANCELLED:provisional
WEBKIT:W5:CHILD:web:12:GONE
WEBKIT:W5:CHILD:network:13:GONE
WEBKIT:W5:QUIT:PASS
"""


def good_record() -> Record:
    record = Record(handshakes=["theoldnet.com"])
    agent = "Mozilla/5.0 (Unknown; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko)"
    record.requests.append(Request("http", "example.com", "GET", "/", 200, agent))
    for path in ["/", *aj.web_judge.page_assets(), "/webkit/tall.html", "/webkit/js.html", "/webkit/links.html",
                 "/webkit/form.html",
                 "/webkit/echo?q=hello+lazyos"]:
        record.requests.append(Request("https", "theoldnet.com", "GET", path, 200, agent, "theoldnet.com"))
    return record


class SerialTests(unittest.TestCase):
    def test_a_good_log_passes(self) -> None:
        self.assertEqual(aj.judge_serial(GOOD), [])

    def test_each_missing_marker_fails(self) -> None:
        for marker in ("WEBKIT:W5:UP\n", "WEBKIT:W5:UI:UP\n", "WEBKIT:W5:WEBPROCESS:UP:12\n",
                       "WEBKIT:W5:NETWORKPROCESS:UP:13\n", f"WEBKIT:W5:LOAD:{aj.OLDNET_URL}\n",
                       f"WEBKIT:W5:TITLE:{aj.OLDNET_TITLE}\n", f"WEBKIT:W5:JS:{aj.JS_TITLE}\n",
                       f"WEBKIT:W5:CLICK:{aj.FORM_URL}\n", f"WEBKIT:W5:INPUT:{aj.ECHO_TITLE}\n"):
            self.assertTrue(aj.judge_serial(GOOD.replace(marker, "")), marker)

    def test_a_failure_line_fails(self) -> None:
        problems = aj.judge_serial(GOOD + "WEBKIT:W5:FAIL:webprocess:crashed\n")
        self.assertTrue(any("crashed" in p for p in problems), problems)
        self.assertTrue(aj.judge_serial(GOOD + "WEBKIT:W5:UI:BIND:FAIL:5\n"))
        # Only the load the session breaks on purpose may fail.
        other = GOOD + "WEBKIT:W5:FAIL:provisional:http://example.com/:Could not connect (code 300)\n"
        self.assertTrue(any("example.com/:" in p for p in aj.judge_serial(other)))

    def test_the_load_that_must_fail_is_reported_and_shown(self) -> None:
        self.assertEqual(aj.judge_errors(GOOD), [])
        failure = f"WEBKIT:W5:FAIL:provisional:{aj.FAIL_URL}:Could not connect to server (code 300)\n"
        self.assertTrue(any("hangs up" in p for p in aj.judge_errors(GOOD.replace(failure, ""))))
        shown = f"WEBKIT:W5:UI:ERROR:Could not load {aj.FAIL_URL}: Could not connect to server\n"
        self.assertTrue(any("never showed" in p for p in aj.judge_errors(GOOD.replace(shown, ""))))

    def test_a_stopped_load_is_cancelled_not_failed(self) -> None:
        self.assertTrue(any("cancelled" in p for p in aj.judge_errors(
            GOOD.replace("WEBKIT:W5:CANCELLED:provisional\n", ""))))
        # Showing the stop as a second error, or letting the slow page finish, fails.
        twice = GOOD + f"WEBKIT:W5:UI:ERROR:Could not load {aj.SLOW_URL}: cancelled\n"
        self.assertTrue(any("more than one" in p for p in aj.judge_errors(twice)))
        self.assertTrue(aj.judge_errors(GOOD + f"WEBKIT:W5:LOAD:{aj.SLOW_URL}\n"))
        self.assertTrue(aj.judge_errors(GOOD.replace(f"WEBKIT:W5:START:{aj.SLOW_URL}\n", "")))

    def test_closing_the_window_ends_both_children(self) -> None:
        self.assertEqual(aj.judge_quit(GOOD), [])
        self.assertTrue(aj.judge_quit(GOOD.replace("WEBKIT:W5:QUIT:PASS\n", "")))
        self.assertTrue(aj.judge_quit(GOOD.replace("WEBKIT:W5:QUIT:PASS", "WEBKIT:W5:QUIT:FAIL")))
        for line in ("WEBKIT:W5:CHILD:web:12:GONE\n", "WEBKIT:W5:CHILD:network:13:GONE\n"):
            self.assertTrue(aj.judge_quit(GOOD.replace(line, "")), line)
        alive = GOOD.replace("WEBKIT:W5:CHILD:web:12:GONE", "WEBKIT:W5:CHILD:web:12:ALIVE")
        self.assertTrue(any("ALIVE" in p for p in aj.judge_quit(alive)))

    def test_a_script_that_never_ran_is_caught_by_the_readback(self) -> None:
        text = GOOD.replace(f"WEBKIT:W5:JS:{aj.JS_TITLE}", "WEBKIT:W5:JS:js-off")
        problems = aj.judge_serial(text)
        self.assertTrue(any("script did not run" in p for p in problems), problems)

    def test_the_wrong_echo_fails(self) -> None:
        text = GOOD.replace(f"WEBKIT:W5:INPUT:{aj.ECHO_TITLE}", "WEBKIT:W5:INPUT:echo:")
        self.assertTrue(any("typed text" in p for p in aj.judge_serial(text)))

    def test_a_window_that_grew_gets_a_larger_page(self) -> None:
        self.assertEqual(aj.resizes(GOOD), [(1000, 498), (1272, 640)])
        self.assertEqual(aj.judge_resize(GOOD), [])
        self.assertTrue(aj.judge_resize(GOOD.replace("WEBKIT:W5:INFO:RESIZE:1272x640\n", "")))
        self.assertTrue(aj.judge_resize(GOOD.replace("1272x640", "1000x498")))

    def test_the_timing_lines_are_read(self) -> None:
        self.assertEqual(aj.first_paint_ms(GOOD), [1900])
        self.assertEqual(aj.load_ms(GOOD), [2000])


class ServerTests(unittest.TestCase):
    def assets(self) -> list[str]:
        return aj.web_judge.page_assets()

    def test_a_good_record_passes(self) -> None:
        self.assertEqual(aj.judge_servers(good_record(), self.assets()), [])

    def test_each_missing_request_fails(self) -> None:
        for path in ("/", "/webkit/js.html", "/webkit/form.html", "/webkit/echo?q=hello+lazyos", *self.assets()):
            record = good_record()
            record.requests = [r for r in record.requests if not (r.scheme == "https" and r.path == path)]
            self.assertTrue(aj.judge_servers(record, self.assets()), path)
        record = good_record()
        record.requests = [r for r in record.requests if r.scheme != "http"]
        self.assertTrue(aj.judge_servers(record, self.assets()))

    def test_a_form_query_with_other_text_does_not_count(self) -> None:
        record = good_record()
        for request in record.requests:
            request.path = request.path.replace("hello+lazyos", "something+else")
        self.assertTrue(any("never reached" in p for p in aj.judge_servers(record, self.assets())))

    def test_sni_must_match_the_host(self) -> None:
        record = good_record()
        record.requests[3].sni = "other.example"
        self.assertTrue(any("SNI" in p for p in aj.judge_servers(record, self.assets())))

    def test_the_user_agent_must_be_webkits(self) -> None:
        record = good_record()
        for request in record.requests:
            request.user_agent = "LazyWeb"
        self.assertTrue(any("WebKit" in p for p in aj.judge_servers(record, self.assets())))

    def test_requests_without_a_user_agent_do_not_pass(self) -> None:
        record = good_record()
        for request in record.requests:
            request.user_agent = ""
        self.assertTrue(any("User-Agent" in p for p in aj.judge_servers(record, self.assets())))

    def test_the_echo_page_escapes_what_was_typed(self) -> None:
        self.assertIn(b"<title>echo:a &lt;b&gt; &amp; c</title>", sites.echo_page("/webkit/echo?q=a+%3Cb%3E+%26+c"))
        self.assertIn(b"<title>echo:</title>", sites.echo_page("/webkit/echo"))

    def test_records_survive_a_round_trip_through_json(self) -> None:
        record = good_record()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "requests.json"
            app_run.save_record(path, record)
            loaded = app_run.load_record(path)
        self.assertEqual([vars(r) for r in loaded.requests], [vars(r) for r in record.requests])
        self.assertEqual(loaded.handshakes, record.handshakes)


def png(path: Path, width: int, height: int, paint) -> Path:
    path.write_bytes(imgenc.png(width, height, [paint(x, y) for y in range(height) for x in range(width)]))
    return path


PROBE = f"""UI:RECT x=100 y=50 w=640 h=480 name=window:{aj.caption('Example Domain')}
UI:WIDGET x=0 y=38 w=640 h=418 name=page window={aj.caption('Example Domain')}
UI:WIDGET x=0 y=38 w=1 h=1 name=page_origin window={aj.caption('Example Domain')}
"""


class ShotTests(unittest.TestCase):
    def test_the_page_area_comes_from_the_probe(self) -> None:
        self.assertEqual(aj.window_region(PROBE), (100, 88, 640, 418))
        self.assertIsNone(aj.window_region("nothing"))
        self.assertIsNone(aj.window_region(PROBE.replace("window:", "windo:")))

    def test_the_page_area_before_and_after_the_window_grew(self) -> None:
        grown = PROBE + (
            "UI:RECT x=0 y=22 w=1280 h=640 name=window:" + aj.caption("Example Domain") + "\n"
            "UI:WIDGET x=0 y=38 w=1280 h=602 name=page window=" + aj.caption("Example Domain") + "\n"
        )
        self.assertEqual(aj.window_region(grown), (100, 88, 640, 418))
        self.assertEqual(aj.window_region(grown, last=True), (0, 60, 1280, 602))

    def test_the_maximized_page_must_be_painted_to_its_right_edge(self) -> None:
        region = (0, 0, 1200, 300)
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            full = png(tmp / "full.png", 1200, 300, lambda x, y: (200, 255, 200))
            short = png(tmp / "short.png", 1200, 300, lambda x, y: (200, 255, 200) if x < 1000 else (255, 255, 255))
            self.assertEqual(aj.judge_maximized(full, region), [])
            self.assertTrue(aj.judge_maximized(short, region))
            self.assertTrue(aj.judge_maximized(full, (0, 0, 800, 300)), "a small page area is not maximized")
            self.assertTrue(aj.judge_maximized(None, region))

    def test_blank_page_areas_fail_and_drawn_ones_pass(self) -> None:
        region = (100, 88, 200, 100)
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)

            def page(n: int):
                # A shot per page: a desktop, with a page area painted differently each time.
                def paint(x: int, y: int):
                    if 100 <= x < 300 and 88 <= y < 188:
                        return ((x * 7 + n * 40) % 256, (y * 5 + n * 70) % 256, (x + y + n * 90) % 256)
                    return (30, 30, 40)
                return paint

            shots = [png(tmp / f"shot_0{n}_theoldnet.png" if n == 2 else tmp / f"shot_0{n}_page.png", 400, 300, page(n))
                     for n in range(1, 8)]
            self.assertEqual(aj.judge_shots(shots, region), [])
            self.assertTrue(aj.judge_shots(shots[:3], region), "too few shots")
            self.assertTrue(aj.judge_shots(shots, None), "no page area")
            blank = png(tmp / "shot_03_page.png", 400, 300, lambda x, y: (255, 255, 255))
            shots[2] = blank
            self.assertTrue(any("blank" in p for p in aj.judge_shots(shots, region)))
            twin = png(tmp / "shot_04_page.png", 400, 300, page(1))
            shots[2] = png(tmp / "shot_03_page.png", 400, 300, page(3))
            shots[3] = twin
            self.assertTrue(any("identical" in p for p in aj.judge_shots(shots, region)))

    def test_the_wheel_must_move_the_bands_by_a_known_number_of_rows(self) -> None:
        region = (0, 0, 120, 400)

        def bands(top: int):
            # Static 50 px bands of different colours, the page `top` rows further down.
            def paint(x: int, y: int):
                band = (y + top) // 50
                return ((band * 37) % 256, (band * 91 + 40) % 256, (band * 53 + 90) % 256)
            return paint

        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            a = png(tmp / "a.png", 120, 400, bands(0))
            b = png(tmp / "b.png", 120, 400, bands(23))
            c = png(tmp / "c.png", 120, 400, bands(130))
            self.assertEqual(aj.judge_scroll({"before": a, "after": b}, region), ([], 23))
            self.assertEqual(aj.judge_scroll({"before": a, "after": c}, region, 130), ([], 130))
            self.assertEqual(aj.SCROLL_PX, 5 * 40, "five notches at WebKit's 40 px line step")
            wrong, shift = aj.judge_scroll({"before": a, "after": b}, region, 40)
            self.assertEqual(shift, 23)
            self.assertTrue(any("expected 40" in p for p in wrong), wrong)
            # The same picture is not a scroll, though static bands look alike a row apart.
            self.assertTrue(aj.judge_scroll({"before": a, "after": a}, region)[0])
            self.assertTrue(aj.judge_scroll({"before": a}, region)[0])

    def test_a_page_that_animates_instead_of_scrolling_does_not_pass(self) -> None:
        region = (0, 0, 120, 400)
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            a = png(tmp / "a.png", 120, 400, lambda x, y: ((x * 3 + y) % 256, (x + y * 5) % 256, 10))
            b = png(tmp / "b.png", 120, 400, lambda x, y: ((x * 7 + y * 3) % 256, (x * 5 + y) % 256, 99))
            self.assertTrue(aj.judge_scroll({"before": a, "after": b}, region)[0])


class SessionTests(unittest.TestCase):
    def test_the_checked_in_example_is_current(self) -> None:
        self.assertEqual(app_session.EXAMPLE.read_text(encoding="utf-8"), app_session.render(app_session.steps()),
                         "run python tools/webkit/app_session.py --write")

    def test_every_page_the_judge_wants_is_visited_and_every_shot_taken(self) -> None:
        text = app_session.render(app_session.steps())
        for url in (aj.EXAMPLE_URL, aj.OLDNET_URL, aj.TALL_URL, aj.JS_URL, aj.LINKS_URL, aj.FAIL_URL, aj.SLOW_URL):
            self.assertIn(url, text)
        # The window is closed from its title bar, and the session waits for the close to finish.
        self.assertIn("WEBKIT:W5:QUIT:", text)
        self.assertIn("WEBKIT:W5:CANCELLED:", text)
        for shot in ("00_window", "01_example", "02_theoldnet", "03_theoldnet_later", "03b_tall", "04_scrolled",
                     "05_js", "06_form", "07_typed", "08_echo", "09_back", "10_failed", "11_stopped",
                     "12_maximized"):
            self.assertIn(f'"shot": "{shot}"', text)
        self.assertIn("mouse_scroll", text)
        self.assertIn(aj.INPUT_TEXT, text)

    def test_clicks_name_windows_by_the_caption_the_app_sets(self) -> None:
        clicks = [s for s in app_session.steps() if "click_at" in s]
        windows = {s["click_at"]["window"] for s in clicks if "window" in s["click_at"]}
        self.assertEqual(windows, {aj.caption(aj.LINKS_TITLE), aj.caption(aj.FORM_TITLE), aj.caption(aj.ECHO_TITLE)})

    def test_the_fixture_pages_have_the_titles_and_places_the_session_assumes(self) -> None:
        root = aj.ROOT / "tools" / "web" / "fixtures" / "theoldnet.com" / "webkit"
        links = (root / "links.html").read_text(encoding="utf-8")
        self.assertIn(f"<title>{aj.LINKS_TITLE}</title>", links)
        self.assertIn("width: 300px; height: 100px", links)
        form = (root / "form.html").read_text(encoding="utf-8")
        self.assertIn(f"<title>{aj.FORM_TITLE}</title>", form)
        self.assertIn("left: 20px; top: 20px; width: 400px; height: 44px", form)
        self.assertIn("<title>js-off</title>", (root / "js.html").read_text(encoding="utf-8"))
        tall = (root / "tall.html").read_text(encoding="utf-8")
        self.assertIn(f"<title>{aj.TALL_TITLE}</title>", tall)
        self.assertIn("height: 50px", tall)
        self.assertNotIn("<script", tall)
        self.assertNotIn("@keyframes", tall)
        self.assertNotIn("animation:", tall)

    def test_the_slow_page_outlasts_the_session_step_that_stops_it(self) -> None:
        self.assertEqual(sites.SLOW_PATH, "/webkit/slow")
        self.assertEqual(aj.SLOW_URL, "https://theoldnet.com" + sites.SLOW_PATH)
        self.assertGreaterEqual(sites.SLOW_SECONDS, 20)

    def test_the_closer_hangs_up_on_whoever_connects(self) -> None:
        import socket
        closer = sites.Closer(0)
        try:
            port = closer.listener.getsockname()[1]
            with socket.create_connection(("127.0.0.1", port), timeout=5) as client:
                client.sendall(b"GET / HTTP/1.1\r\nHost: example.com\r\n\r\n")
                try:
                    self.assertEqual(client.recv(100), b"")
                except ConnectionResetError:
                    pass  # a hang-up with the request unread is a reset on some hosts: a failure all the same
        finally:
            closer.close()

    def test_the_run_fails_on_every_failure_but_the_planned_one(self) -> None:
        import re
        pattern = re.compile(r"WEBKIT:W5:FAIL:(?!provisional:" + re.escape(aj.FAIL_URL) + ":)")
        self.assertIsNone(pattern.search(f"WEBKIT:W5:FAIL:provisional:{aj.FAIL_URL}:refused (code 300)"))
        self.assertTrue(pattern.search("WEBKIT:W5:FAIL:webprocess:crashed"))
        self.assertTrue(pattern.search("WEBKIT:W5:FAIL:provisional:http://example.com/:refused"))


if __name__ == "__main__":
    unittest.main()
