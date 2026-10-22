#!/usr/bin/env python3
"""The notification judge fails when it should (`notify_judge.py`), and the session scripts that
drive the harness are the checked-in examples."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import notify_judge as n  # noqa: E402
import notify_session  # noqa: E402

P = n.P
N = n.j.NOTE
WEBD_UP = n.j.WEBD_UP


def boot(*extra: str) -> list[str]:
    """What every boot's log has: webd up and its view made, the browser's first frame, and both engine
    processes up (the engine's markers are webd's relay)."""
    return [WEBD_UP, f"WEBD:VIEW:CREATE client={n.j.CLIENT_LABEL} view=3", f"{P}:UP:PASS",
            f"{N}WEBPROCESS:UP:101", f"{N}NETWORKPROCESS:UP:102", *extra]


def allow_log(**changes) -> str:
    """A first boot as it should look; `changes` replace one part by name."""
    parts = {
        "start": boot(f"{P}:LOAD:{n.PAGE}", f"{P}:TITLE:{n.READY_DEFAULT}"),
        "ask": [f"{P}:NOTIFY:ASK:{n.ORIGIN}", f"{P}:NOTIFY:ALLOW:{n.ORIGIN}", f"{P}:TITLE:{n.PERM_GRANTED}"],
        "toast": [
            f"{P}:NOTIFY:SHOW:{n.ORIGIN}:7",
            "SHELL:NOTIFY:POST owner=LazyWeb+ id=1 level=info title=theoldnet.com",
            "SHELL:NOTIFY:SHOW id=1",
            f"{P}:NOTIFY:SHOW:{n.ORIGIN}:8",
            "SHELL:NOTIFY:POST owner=LazyWeb+ id=1 level=info title=theoldnet.com",
        ],
        "click": [f"{P}:NOTIFY:CLICK:8", f"{P}:TITLE:{n.CLICKED_SECOND}", "XUID:WM:MINIMIZE id=3 windows=1",
                  f"{P}:NOTIFY:CLICK:10", "XUID:WM:UNMINIMIZE id=3 windows=1", f"{P}:TITLE:{n.CLICKED_LATER}"],
        "burst": [f"{P}:NOTIFY:DROP:{n.ORIGIN}:site"] * 4,
        "back": [f"{P}:NOTIFY:CLICK:9", f"{P}:LOAD:{n.PAGE}"],
    }
    parts.update(changes)
    return "\n".join(line for part in parts.values() for line in part) + "\n"


def kept_log(**changes) -> str:
    parts = {
        "start": boot(f"{P}:LOAD:{n.PAGE}", f"{P}:TITLE:{n.READY_GRANTED}"),
        "toast": [f"{P}:NOTIFY:SHOW:{n.ORIGIN}:3",
                  "SHELL:NOTIFY:POST owner=LazyWeb+ id=1 level=info title=theoldnet.com"],
        "www": [f"{P}:LOAD:{n.WWW_PAGE}", f"{P}:NOTIFY:ASK:{n.WWW_ORIGIN}", f"{P}:NOTIFY:BLOCK:{n.WWW_ORIGIN}",
                f"{P}:TITLE:{n.PERM_DENIED}"],
        "about": [f"{P}:LOAD:about:lazyweb", f"{P}:NOTIFY:FORGET:{n.WWW_ORIGIN}"],
    }
    parts.update(changes)
    return "\n".join(line for part in parts.values() for line in part) + "\n"


class AllowBoot(unittest.TestCase):
    def test_a_good_first_boot_passes(self):
        self.assertEqual(n.judge_allow(allow_log()), [])

    def assertFails(self, text: str, needle: str):
        problems = n.judge_allow(text)
        self.assertTrue(any(needle in p for p in problems), f"{needle!r} not in {problems}")

    def test_a_page_that_asks_without_a_gesture_is_not_asked_about(self):
        text = allow_log(ask=[f"{P}:NOTIFY:ASK:{n.ORIGIN}"] * 2
                         + [f"{P}:NOTIFY:ALLOW:{n.ORIGIN}", f"{P}:TITLE:{n.PERM_GRANTED}"])
        self.assertFails(text, "exactly one")

    def test_the_toast_must_be_titled_with_the_site_never_the_pages_words(self):
        text = allow_log(toast=[
            f"{P}:NOTIFY:SHOW:{n.ORIGIN}:7",
            "SHELL:NOTIFY:POST owner=LazyWeb+ id=1 level=info title=Windows Security Alert",
            "SHELL:NOTIFY:SHOW id=1",
            f"{P}:NOTIFY:SHOW:{n.ORIGIN}:8",
            "SHELL:NOTIFY:POST owner=LazyWeb+ id=1 level=info title=theoldnet.com",
        ])
        self.assertFails(text, "Windows Security Alert")

    def test_a_page_cannot_choose_the_level(self):
        text = allow_log(toast=[
            f"{P}:NOTIFY:SHOW:{n.ORIGIN}:7",
            "SHELL:NOTIFY:POST owner=LazyWeb+ id=1 level=error title=theoldnet.com",
            "SHELL:NOTIFY:SHOW id=1",
            f"{P}:NOTIFY:SHOW:{n.ORIGIN}:8",
            "SHELL:NOTIFY:POST owner=LazyWeb+ id=1 level=info title=theoldnet.com",
        ])
        self.assertFails(text, "level error")

    def test_a_tag_that_made_a_second_toast_fails(self):
        text = allow_log(toast=[
            f"{P}:NOTIFY:SHOW:{n.ORIGIN}:7",
            "SHELL:NOTIFY:POST owner=LazyWeb+ id=1 level=info title=theoldnet.com",
            "SHELL:NOTIFY:SHOW id=1",
            f"{P}:NOTIFY:SHOW:{n.ORIGIN}:8",
            "SHELL:NOTIFY:POST owner=LazyWeb+ id=2 level=info title=theoldnet.com",
        ])
        self.assertFails(text, "second toast")

    def test_a_burst_that_was_not_cut_fails(self):
        self.assertFails(allow_log(burst=[f"{P}:NOTIFY:DROP:{n.ORIGIN}:rate"]), "burst of six was not cut")

    def test_the_shells_own_limit_means_the_browsers_did_not_hold(self):
        self.assertFails(allow_log(burst=[f"{P}:NOTIFY:DROP:{n.ORIGIN}:site"] * 4
                                   + ["SHELL:NOTIFY:LIMIT owner=LazyWeb+"]), "rate limit")

    def test_a_click_that_did_not_raise_the_minimized_window_fails(self):
        text = allow_log().replace("XUID:WM:UNMINIMIZE id=3 windows=1\n", "")
        self.assertFails(text, "bring the minimized window back")

    def test_a_click_that_did_not_bring_the_page_back_fails(self):
        self.assertFails(allow_log(back=[f"{P}:NOTIFY:CLICK:9"]), "bring it back")

    def test_a_click_that_never_reached_the_handler_fails(self):
        self.assertFails(allow_log(click=[f"{P}:NOTIFY:CLICK:8", "XUID:WM:MINIMIZE id=3 windows=1",
                                          "XUID:WM:UNMINIMIZE id=3 windows=1", f"{P}:NOTIFY:CLICK:9",
                                          f"{P}:NOTIFY:CLICK:10"]), "handler")

    def test_nothing_from_the_browser_in_the_shell_fails(self):
        text = allow_log(toast=[f"{P}:NOTIFY:SHOW:{n.ORIGIN}:7", f"{P}:NOTIFY:SHOW:{n.ORIGIN}:8"])
        self.assertFails(text, "no toast from the browser")

    def test_a_toast_from_the_terminal_is_not_the_browsers(self):
        posts = n.web_posts("SHELL:NOTIFY:POST owner=<terminal> id=5 level=info title=hi\n")
        self.assertEqual(posts, [])

    def test_the_child_processes_must_have_come_up(self):
        text = allow_log().replace(f"{N}WEBPROCESS:UP:101\n", "")
        self.assertFails(text, "WEBPROCESS")
        # The engine's lines under the client's prefix are not the engine's.
        self.assertFails(allow_log().replace(f"{N}WEBPROCESS", f"{P}:WEBPROCESS"), "WEBPROCESS")

    def test_webd_must_have_come_up_and_made_the_view(self):
        self.assertFails(allow_log().replace(f"{WEBD_UP}\n", ""), WEBD_UP)
        self.assertFails(allow_log().replace(f"WEBD:VIEW:CREATE client={n.j.CLIENT_LABEL} view=3\n", ""), "WEBD:VIEW:CREATE")

    def test_a_toast_posted_by_webd_is_not_the_browsers(self):
        text = allow_log(toast=[
            f"{P}:NOTIFY:SHOW:{n.ORIGIN}:7",
            "SHELL:NOTIFY:POST owner=os.lazy.webd id=1 level=info title=theoldnet.com",
            "SHELL:NOTIFY:SHOW id=1",
            f"{P}:NOTIFY:SHOW:{n.ORIGIN}:8",
            "SHELL:NOTIFY:POST owner=os.lazy.webd id=1 level=info title=theoldnet.com",
        ])
        self.assertFails(text, "web engine daemon")

    def test_another_program_started_on_a_pages_say_fails(self):
        self.assertFails(allow_log() + f"{P}:LAUNCH:mailto:x@y:OK\n", "started another program")


class KeptBoot(unittest.TestCase):
    def test_a_good_second_boot_passes(self):
        self.assertEqual(n.judge_kept(kept_log()), [])

    def assertFails(self, text: str, needle: str):
        problems = n.judge_kept(text)
        self.assertTrue(any(needle in p for p in problems), f"{needle!r} not in {problems}")

    def test_an_allowed_site_that_is_asked_about_again_fails(self):
        text = kept_log(toast=[f"{P}:NOTIFY:ASK:{n.ORIGIN}", f"{P}:NOTIFY:SHOW:{n.ORIGIN}:3",
                               "SHELL:NOTIFY:POST owner=LazyWeb+ id=1 level=info title=theoldnet.com"])
        self.assertFails(text, "asked about again")

    def test_a_blocked_site_that_shows_a_toast_fails(self):
        text = kept_log(www=[f"{P}:LOAD:{n.WWW_PAGE}", f"{P}:NOTIFY:ASK:{n.WWW_ORIGIN}",
                             f"{P}:NOTIFY:BLOCK:{n.WWW_ORIGIN}", f"{P}:TITLE:{n.PERM_DENIED}",
                             f"{P}:NOTIFY:SHOW:{n.WWW_ORIGIN}:4",
                             "SHELL:NOTIFY:POST owner=LazyWeb+ id=2 level=info title=www.theoldnet.com"])
        problems = n.judge_kept(text)
        self.assertTrue(any("blocked site showed" in p for p in problems), problems)
        self.assertTrue(any("shell saw a toast for the blocked site" in p for p in problems), problems)

    def test_a_forget_that_never_happened_fails(self):
        self.assertFails(kept_log(about=[f"{P}:LOAD:about:lazyweb"]), "not forgotten")


class Profile(unittest.TestCase):
    ALLOWED = f"{n.ORIGIN}\tallow\t1791257400\n"
    BOTH = ALLOWED + f"{n.WWW_ORIGIN}\tblock\t1791257460\n"

    def test_the_documented_file_passes(self):
        self.assertEqual(n.judge_profile(self.ALLOWED, self.ALLOWED), [])

    def test_no_file_after_the_first_boot_means_nothing_was_kept(self):
        self.assertTrue(n.judge_profile(None, None))

    def test_a_blocked_site_that_stayed_after_forget_fails(self):
        problems = n.judge_profile(self.ALLOWED, self.BOTH)
        self.assertTrue(any("forgotten" in p for p in problems), problems)

    def test_a_time_that_is_not_seconds_fails(self):
        self.assertTrue(n.judge_profile(f"{n.ORIGIN}\tallow\tyesterday\n", self.ALLOWED))

    def test_extra_sites_after_the_first_boot_fail(self):
        self.assertTrue(n.judge_profile(self.BOTH, self.ALLOWED))


class Sessions(unittest.TestCase):
    def test_the_checked_in_examples_are_current(self):
        for path, steps in ((notify_session.EXAMPLE_ALLOW, notify_session.allow()),
                            (notify_session.EXAMPLE_KEPT, notify_session.kept())):
            self.assertTrue(path.is_file(), f"{path} is missing: python tools/webplus/notify_session.py --write")
            self.assertEqual(path.read_text(encoding="utf-8"), notify_session.render(steps),
                             f"{path} is stale: python tools/webplus/notify_session.py --write")

    def test_every_session_ends_by_closing_the_window_and_quitting(self):
        for steps in (notify_session.allow(), notify_session.kept()):
            self.assertEqual(steps[-1], {"quit": True})
            self.assertTrue(any(f"{P}:QUIT:" == step.get("until") for step in steps))

    def test_the_session_answers_the_bar_by_the_names_the_window_announces(self):
        steps = notify_session.allow() + notify_session.kept()
        widgets = {step["click_at"].get("widget") for step in steps if isinstance(step.get("click_at"), dict)}
        self.assertTrue({"notify_allow", "notify_block"} <= widgets, widgets)


if __name__ == "__main__":
    unittest.main()
