#!/usr/bin/env python3
"""The LazyWeb+ harness's own tests: the judge fails when it should (a script that started
another program, a dialog the page never heard back from, a cookie that did not survive, a download
cut short, a child that outlived the window), the checked-in sessions are current, and the stand-in
sites answer the way the judge expects.

    python tools/webplus/test_judge.py
"""

from __future__ import annotations

import json
import re
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import denies  # noqa: E402
import judge as j  # noqa: E402
import pixels  # noqa: E402
import run  # noqa: E402
import session  # noqa: E402
import sites  # noqa: E402

from judge_logs import CREATE, DESTROY, N, P, good_first, log, page  # noqa: E402,F401
from test_quit_judge import QuitTests  # noqa: E402,F401  (run with this file)


class SerialTests(unittest.TestCase):
    def test_a_good_first_boot_passes_every_serial_judge(self) -> None:
        text = good_first()
        for judge in (j.judge_first, j.judge_quit, j.judge_resize):
            self.assertEqual(judge(text), [], judge.__name__)

    def test_a_missing_page_or_title_is_named(self) -> None:
        text = good_first().replace(f"{P}:TITLE:{j.OLDNET_TITLE}\n", "")
        self.assertTrue(any(j.OLDNET_TITLE in p for p in j.judge_pages(text)))
        text = good_first().replace(f"{P}:LOAD:{j.COOKIE_URL}\n", "")
        self.assertTrue(any(j.COOKIE_URL in p for p in j.judge_pages(text)))

    def test_an_unplanned_failure_fails_the_run_and_the_planned_one_must_happen(self) -> None:
        text = good_first() + f"{P}:FAIL:Could not load http://elsewhere/: nope\n"
        self.assertTrue(any("elsewhere" in p for p in j.judge_pages(text)))
        missing = good_first().replace(f"{P}:FAIL:Could not load {j.FAIL_URL}: Server returned nothing\n", "")
        self.assertTrue(any("hangs up" in p for p in j.judge_pages(missing)))

    def test_a_stopped_page_that_loaded_anyway_is_a_failure(self) -> None:
        text = good_first() + f"{P}:LOAD:{j.SLOW_URL}\n"
        self.assertTrue(any("stopped page loaded" in p for p in j.judge_pages(text)))
        self.assertTrue(any("STOPPED" in p for p in j.judge_pages(good_first().replace(f"{P}:STOPPED\n", ""))))

    def test_the_dialogs_must_come_in_order_and_their_answers_reach_the_page(self) -> None:
        self.assertEqual(j.judge_dialogs(good_first()), [])
        swapped = good_first().replace(f"{P}:DIALOG:confirm\n", "").replace(f"{P}:DIALOG:prompt\n", "") \
            + f"{P}:DIALOG:prompt\n{P}:DIALOG:confirm\n"
        self.assertTrue(any("order" in p for p in j.judge_dialogs(swapped)))
        unanswered = good_first().replace(f"{P}:TITLE:{j.SCRIPT_TITLE}\n", f"{P}:TITLE:script-ok:no:None\n")
        self.assertTrue(any("answers did not reach" in p for p in j.judge_dialogs(unanswered)))
        silent = good_first().replace(f"{P}:DIALOG:alert\n", "")
        self.assertTrue(any("DIALOG:alert" in p for p in j.judge_dialogs(silent)))


class LaunchTests(unittest.TestCase):
    def test_a_script_that_starts_another_program_fails_the_run(self) -> None:
        # The policy let the script's mailto: through: mimed was asked.
        text = good_first().replace(f"{P}:LAUNCH:{j.EVIL_MAILTO}:BLOCKED", f"{P}:LAUNCH:{j.EVIL_MAILTO}:OK")
        problems = j.judge_launches(text)
        self.assertTrue(any("not refused" in p for p in problems), problems)
        self.assertTrue(any("started something" in p for p in problems), problems)

    def test_a_browser_command_that_was_not_blocked_fails_the_run(self) -> None:
        text = good_first().replace(f"{P}:LAUNCH:{j.COMMAND_URL}:BLOCKED\n", "")
        self.assertTrue(any(j.COMMAND_URL in p for p in j.judge_launches(text)))

    def test_the_users_click_must_reach_mimed_and_not_be_refused(self) -> None:
        text = good_first().replace(f"{P}:LAUNCH:{j.MAIL_URL}:FAIL", f"{P}:LAUNCH:{j.MAIL_URL}:BLOCKED")
        problems = j.judge_launches(text)
        self.assertTrue(any("user gesture" in p for p in problems), problems)
        gone = good_first().replace(f"{P}:LAUNCH:{j.MAIL_URL}:FAIL\n", "")
        self.assertTrue(any("did not reach mimed" in p for p in j.judge_launches(gone)))
        # No Mail in the image: FAIL is as good as OK (the app was asked).
        ok = good_first().replace(f"{P}:LAUNCH:{j.MAIL_URL}:FAIL", f"{P}:LAUNCH:{j.MAIL_URL}:OK")
        self.assertEqual(j.judge_launches(ok), [])

    def test_a_window_open_without_a_click_is_not_allowed_to_open_a_window(self) -> None:
        self.assertEqual(j.judge_popups(good_first()), [])
        twice = good_first() + f"{P}:POPUP:{j.OPENED_URL}\n"
        self.assertTrue(any("without a click" in p for p in j.judge_popups(twice)))
        none = good_first().replace(f"{P}:POPUP:{j.BLANK_URL}\n", "")
        self.assertTrue(any(j.BLANK_URL in p for p in j.judge_popups(none)))


class DownloadTests(unittest.TestCase):
    def test_both_downloads_finish_whole(self) -> None:
        self.assertEqual(j.judge_downloads(good_first()), [])

    def test_a_short_or_failed_download_is_named(self) -> None:
        short = good_first().replace(f":{j.DOWNLOAD_BYTES}\n", ":1000\n")
        self.assertTrue(any(j.DOWNLOAD_NAME in p for p in j.judge_downloads(short)))
        failed = good_first() + f"{P}:DOWNLOAD:FAIL:Cancelled\n"
        self.assertTrue(any("Cancelled" in p for p in j.judge_downloads(failed)))

    def test_the_payloads_are_what_the_judge_counts(self) -> None:
        self.assertEqual(j.DOWNLOAD_BYTES, 300 * 1024)
        self.assertEqual(j.SLOW_BYTES, sites.SLOW_DOWNLOAD_BYTES)
        self.assertEqual(len(sites.slow_download_payload()), j.SLOW_BYTES)


def request(path: str, cookie: str = "", scheme: str = "https", host: str = "theoldnet.com", status: int = 200):
    return sites.Request(scheme, host, "GET", path, status, "Mozilla/5.0 AppleWebKit/605.1.15", host if scheme == "https" else None,
                         cookie)


def served(cookie_header: str, extra: list | None = None) -> sites.Record:
    paths = ["/webplus/script.html", "/webplus/links.html", "/webplus/blank-target.html", "/webplus/opened.html",
             sites.DOWNLOAD_PATH, sites.SLOW_DOWNLOAD_PATH, "/webkit/tall.html", "/webkit/links.html",
             "/webkit/form.html", f"/webkit/echo?q={j.INPUT_TEXT.replace(' ', '+')}", "/"]
    requests = [request(path) for path in paths]
    requests += [request("/", scheme="http", host="example.com"), request(sites.COOKIE_PATH, cookie_header)]
    requests += extra or []
    return sites.Record(requests, ["theoldnet.com"])


class ServerTests(unittest.TestCase):
    ASSETS = ["/style.css"]

    def judge(self, first: sites.Record, second: sites.Record) -> list[str]:
        return j.judge_servers(first, second, self.ASSETS)

    def test_the_cookie_is_new_on_the_first_boot_and_comes_back_on_the_second(self) -> None:
        first = served("", [request("/style.css")])
        second = sites.Record([request(sites.COOKIE_PATH, f"{sites.COOKIE_NAME}={sites.COOKIE_VALUE}")], [])
        self.assertEqual(self.judge(first, second), [])

    def test_a_page_the_cache_should_have_served_is_named(self) -> None:
        first = served("", [request("/style.css")])
        again = sites.Record([request("/"), request(sites.COOKIE_PATH, f"{sites.COOKIE_NAME}={sites.COOKIE_VALUE}")], [])
        problems = self.judge(first, again)
        self.assertTrue(any("HTTP cache" in p for p in problems), problems)
        picture = sites.Record([request("/style.css"), request(sites.COOKIE_PATH,
                                                              f"{sites.COOKIE_NAME}={sites.COOKIE_VALUE}")], [])
        self.assertTrue(any("/style.css" in p and "HTTP cache" in p for p in self.judge(first, picture)))

    def test_a_profile_that_forgot_the_cookie_is_named(self) -> None:
        first = served("", [request("/style.css")])
        second = sites.Record([request(sites.COOKIE_PATH, "")], [])
        self.assertTrue(any("did not keep it" in p for p in self.judge(first, second)))
        silent = sites.Record([], [])
        self.assertTrue(any("never asked" in p for p in self.judge(first, silent)))

    def test_a_cookie_that_was_there_before_it_was_set_is_a_harness_problem(self) -> None:
        first = served(f"{sites.COOKIE_NAME}=stale", [request("/style.css")])
        second = sites.Record([request(sites.COOKIE_PATH, f"{sites.COOKIE_NAME}={sites.COOKIE_VALUE}")], [])
        self.assertTrue(any("before it had set one" in p for p in self.judge(first, second)))

    def test_a_missing_page_or_query_is_named(self) -> None:
        first = served("", [request("/style.css")])
        first.requests = [r for r in first.requests if "/webplus/links.html" not in r.path and "echo" not in r.path]
        second = sites.Record([request(sites.COOKIE_PATH, f"{sites.COOKIE_NAME}={sites.COOKIE_VALUE}")], [])
        problems = self.judge(first, second)
        self.assertTrue(any("links.html" in p for p in problems), problems)
        self.assertTrue(any("echo" in p for p in problems), problems)

    def test_requests_must_look_like_webkits(self) -> None:
        first = served("", [request("/style.css")])
        for r in first.requests:
            r.user_agent = "curl/8"
        second = sites.Record([request(sites.COOKIE_PATH, f"{sites.COOKIE_NAME}={sites.COOKIE_VALUE}")], [])
        self.assertTrue(any("WebKit" in p for p in self.judge(first, second)))


class SiteTests(unittest.TestCase):
    def test_the_cookie_page_names_the_value_it_received(self) -> None:
        self.assertIn(b"<title>cookie:none</title>", sites.cookie_page(""))
        self.assertIn(b"<title>cookie:none</title>", sites.cookie_page("other=1"))
        self.assertIn(f"<title>cookie:{sites.COOKIE_VALUE}</title>".encode(),
                      sites.cookie_page(f"a=b; {sites.COOKIE_NAME}={sites.COOKIE_VALUE}"))
        self.assertIn(b"cookie:&lt;b&gt;", sites.cookie_page(f"{sites.COOKIE_NAME}=<b>"))

    def test_the_fixtures_the_session_uses_exist_with_their_titles(self) -> None:
        base = sites.FIXTURES / "theoldnet.com" / "webplus"
        for name, title in (("links.html", j.LINKS_TITLE), ("blank-target.html", j.BLANK_TITLE),
                            ("opened.html", j.OPENED_TITLE)):
            self.assertIn(f"<title>{title}</title>", (base / name).read_text(encoding="utf-8"), name)
        script = (base / "script.html").read_text(encoding="utf-8")
        for needle in ("alert(", "confirm(", "prompt(", j.EVIL_MAILTO, j.COMMAND_URL, "window.open"):
            self.assertIn(needle, script, needle)
        self.assertIn("script-ok:", script)


class ProfileTests(unittest.TestCase):
    def listing(self, **override) -> dict:
        files = {f"Downloads/{j.DOWNLOAD_NAME}": b"x" * j.DOWNLOAD_BYTES, f"Downloads/{j.SLOW_NAME}": b"y" * j.SLOW_BYTES,
                 f"Downloads/{j.DOWNLOAD_NAME}.part": None, f"Downloads/{j.SLOW_NAME}.part": None,
                 j.COOKIE_KEY: b"SQLite format 3"}
        files.update(override)
        return files

    def history(self, *urls: str) -> str:
        return "".join(f"{1000 + n}\t{url}\ttitle\n" for n, url in enumerate(urls))

    FIRST = (j.EXAMPLE_URL, j.OLDNET_URL, j.COOKIE_URL, j.SCRIPT_URL, j.LINKS_URL)

    def test_a_good_profile_passes(self) -> None:
        self.assertEqual(j.judge_profile(self.listing(), self.history(*self.FIRST), ""), [])

    def test_a_page_missing_from_the_history_is_named(self) -> None:
        problems = j.judge_profile(self.listing(), self.history(*self.FIRST[:-1]), "")
        self.assertTrue(any(j.LINKS_URL in p for p in problems), problems)

    def test_the_browsers_own_pages_never_enter_the_history(self) -> None:
        problems = j.judge_profile(self.listing(), self.history(*self.FIRST, "about:history"), "")
        self.assertTrue(any("own pages" in p for p in problems), problems)

    def test_a_cut_download_a_left_part_file_and_no_cookie_database_are_named(self) -> None:
        listing = self.listing(**{f"Downloads/{j.DOWNLOAD_NAME}": b"x" * 10,
                                  f"Downloads/{j.SLOW_NAME}.part": b"y",
                                  j.COOKIE_KEY: None})
        problems = j.judge_profile(listing, self.history(*self.FIRST), "")
        self.assertTrue(any("is 10 bytes" in p for p in problems), problems)
        self.assertTrue(any(".part was left" in p for p in problems), problems)
        self.assertTrue(any(j.COOKIE_DB in p for p in problems), problems)

    def test_the_cookie_jar_is_in_webds_browser_profile_not_the_apps_folder(self) -> None:
        self.assertEqual(j.COOKIE_KEY, ".apps/os.lazy.webd/profiles/browser/cookies/cookie.jar.db")
        old = self.listing(**{j.COOKIE_KEY: None, f".apps/os.lazy.lazywebplus/{j.COOKIE_DB}": b"SQLite format 3"})
        self.assertTrue(any(j.COOKIE_DB in p for p in j.judge_profile(old, self.history(*self.FIRST), "")))

    def test_clear_history_must_leave_the_file_empty(self) -> None:
        problems = j.judge_profile(self.listing(), self.history(*self.FIRST), self.history(j.EXAMPLE_URL))
        self.assertTrue(any("Clear history" in p for p in problems), problems)


class SecondBootTests(unittest.TestCase):
    def good(self) -> str:
        return log(j.WEBD_UP, CREATE, f"{P}:UP:PASS", f"{N}PROCESS:LAUNCH:web:5", f"{N}WEBPROCESS:UP:5",
                   f"{N}NETWORKPROCESS:UP:6",
                   *page(j.OLDNET_URL, j.OLDNET_TITLE),
                   *page(j.COOKIE_URL, j.COOKIE_KEPT), *page("about:history", "History"),
                   DESTROY, f"{N}PROCESS:EXIT:web:5:signal=9", f"{P}:QUIT:PASS")

    def test_the_cookie_must_come_back(self) -> None:
        self.assertEqual(j.judge_second(self.good()), [])
        gone = self.good().replace(j.COOKIE_KEPT, j.COOKIE_NONE)
        self.assertTrue(any("did not come back" in p for p in j.judge_second(gone)))

    def test_the_cached_page_must_load(self) -> None:
        gone = self.good().replace(j.OLDNET_TITLE, "Could not load")
        self.assertTrue(any("from the cache" in p for p in j.judge_second(gone)), j.judge_second(gone))

    def test_a_task_killed_by_a_fault_is_named_in_either_boot(self) -> None:
        crash = "user: task 77 killed by general protection fault (rip 0x3d5ef54), status 139\n"
        for judged in (j.judge_second(self.good() + crash), j.judge_start(good_first() + crash)):
            self.assertTrue(any("died of a fault" in p and "task 77" in p for p in judged), judged)
        self.assertEqual(j.judge_no_fault(self.good()), [])

    def test_the_second_boot_starts_no_program(self) -> None:
        text = self.good() + f"{P}:LAUNCH:{j.COMMAND_URL}:BLOCKED\n"
        self.assertTrue(any("another program" in p for p in j.judge_second(text)))


class PixelTests(unittest.TestCase):
    def rows(self, bands: list[int], width: int = 4) -> list[bytes]:
        return [bytes([band]) * width for band in bands]

    def test_a_scroll_is_the_shift_that_lines_the_bands_up(self) -> None:
        bands = [i // 10 % 7 for i in range(400)]
        before = self.rows(bands)
        after = self.rows(bands[37:] + [0] * 37)
        self.assertEqual(pixels.scroll_offset(before, after), 37)

    def test_a_page_that_did_not_move_is_not_a_scroll(self) -> None:
        bands = [i // 10 % 7 for i in range(400)]
        self.assertIsNone(pixels.scroll_offset(self.rows(bands), self.rows(bands)))

    def test_a_missing_screenshot_is_a_problem_not_a_pass(self) -> None:
        problems, shift = pixels.judge_scroll({"before": None, "after": None}, (0, 0, 10, 10))
        self.assertTrue(problems and shift is None)
        self.assertTrue(pixels.judge_maximized(None, None))


class SessionTests(unittest.TestCase):
    def test_the_checked_in_sessions_are_current(self) -> None:
        self.assertEqual(session.main_check(), 0, "run python tools/webplus/session.py --write")

    def test_every_marker_a_session_waits_for_is_one_the_judge_reads(self) -> None:
        for steps in (session.first(), session.second()):
            for step in steps:
                for field in ("wait_for", "until"):
                    marker = step.get(field, "")
                    # The client's markers, or an engine marker as webd relays it (WEBD:NOTE:...).
                    if marker.startswith((P, j.NOTE)):
                        self.assertRegex(marker, rf"^(?:{P}|{re.escape(j.NOTE)}):?[A-Z]+", marker)

    def test_the_second_boot_does_not_rebuild_the_world(self) -> None:
        text = json.dumps(session.second())
        self.assertNotIn("DOWNLOAD", text)
        self.assertIn("COOKIE", text.upper())

    def test_a_session_ends_after_the_flusher_has_run(self) -> None:
        # The profile is read from the OS volume after a stop that does not sync: wait past 5 s.
        for steps in (session.first(), session.second()):
            self.assertEqual(steps[-1], {"quit": True})
            self.assertGreaterEqual(steps[-2].get("wait", 0), 8.0)


class RunTests(unittest.TestCase):
    def test_records_round_trip_through_the_run_directory(self) -> None:
        record = served(f"{sites.COOKIE_NAME}=1")
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "requests.json"
            run.save_record(path, record)
            loaded = run.load_record(path)
        self.assertEqual([vars(r) for r in loaded.requests], [vars(r) for r in record.requests])
        self.assertEqual(loaded.handshakes, record.handshakes)

    def test_the_image_is_built_with_the_probe_the_autostart_and_the_big_app_limits(self) -> None:
        source = (HERE / "run.py").read_text(encoding="utf-8")
        for needle in ("LAZYOS_LAZYWEBPLUS", "LAZYOS_XUI_AUTOSTART=\"lazywebplus\"", "LAZYOS_UI_PROBE", "WEBKIT_LIMITS"):
            self.assertIn(needle, source, needle)

    def test_the_browser_is_built_with_webd_never_without_it(self) -> None:
        # LAZYOS_LAZYWEBPLUS=1 requires webd; LAZYOS_WEBD=0 beside it is a build error.
        source = (HERE / "run.py").read_text(encoding="utf-8")
        self.assertNotIn('LAZYOS_WEBD="0"', source)
        self.assertIn('"LAZYOS_WEBD"', source, "an inherited LAZYOS_WEBD=0 is removed from the build's environment")

    def test_the_build_needs_the_client_webd_and_the_engine_processes(self) -> None:
        names = ["xui-lazywebplus.elf", "xui-webd.elf", "WebProcess", "NetworkProcess", "fetch.elf"]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            saved = (run.ROOT, run.BIN, run.ELF, run.WEBD_ELF, run.CURL_ELF)
            try:
                run.ROOT, run.BIN = root, root
                run.ELF, run.WEBD_ELF, run.CURL_ELF = root / names[0], root / names[1], root / names[4]
                missing = run.built()
                self.assertTrue(all(name in missing for name in names), missing)
                self.assertNotIn("lazywebplus-app", missing)
                for name in names:
                    (root / name).write_bytes(b"x")
                self.assertIsNone(run.built())
                (root / "xui-webd.elf").unlink()
                self.assertIn("xui-webd.elf", run.built())
            finally:
                run.ROOT, run.BIN, run.ELF, run.WEBD_ELF, run.CURL_ELF = saved



class DenyTests(unittest.TestCase):
    # The engine's processes are webd's children now (webd's package tables).
    PAGE = f"LABEL:DENY label=app:{denies.ENGINE_APP}/webprocess"
    NET = f"LABEL:DENY label=app:{denies.ENGINE_APP}/networkprocess"

    def test_the_documented_optional_denials_pass(self) -> None:
        lines = [f"{self.PAGE} file=/etc access=stat", f"{self.PAGE} file=/usr/share/zoneinfo access=stat",
                 f"{self.PAGE} file=/build/webkit-deps/prefix/share/fontconfig/conf.avail access=read",
                 f"{self.PAGE} file=/system/share/fonts/liberation access=write", f"{self.PAGE} file=/tmp access=write",
                 f"{self.NET} file=/etc/ssl/openssl.cnf access=stat"]
        self.assertEqual(denies.unexpected(lines), [])

    def test_anything_else_is_a_missing_permission_or_an_attack(self) -> None:
        for line in (f"{self.PAGE} file=/home/user/.profile access=stat",
                     f"{self.PAGE} net=inet",
                     f"{self.NET} file=/etc/localtime access=stat",   # the other child's optional lookup
                     f"LABEL:DENY label=app:{denies.APP} iface=0x1 method=2",
                     f"LABEL:DENY label=app:{denies.APP}/listupdate file=/etc access=stat",
                     f"LABEL:DENY label=app:{denies.ENGINE_APP} iface=0x1 method=2",
                     # The old in-process engine's labels: LazyWeb+ has no engine children any more.
                     f"LABEL:DENY label=app:{denies.APP}/webprocess file=/etc access=stat",
                     f"LABEL:DENY label=app:{denies.APP}/networkprocess file=/etc/ssl/openssl.cnf access=stat",
                     "LABEL:DENY label=app:os.lazy.other/webprocess file=/etc access=stat"):
            self.assertEqual(denies.unexpected([line]), [line], line)


if __name__ == "__main__":
    unittest.main()
