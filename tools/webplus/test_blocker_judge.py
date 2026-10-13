#!/usr/bin/env python3
"""The ad blocker's judge (`blocker.py`) fails when it should: each failure it exists to catch is
fed to it, and a run that is right passes. No QEMU, no build.

Run: python tools/webplus/test_blocker_judge.py
"""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import blocker as b  # noqa: E402
import judge as j  # noqa: E402

sites = j.sites
P = j.P


def log(*lines: str) -> str:
    return "\n".join(lines) + "\n"


FIRST = log(
    f"{P}:BLOCK:UP",
    f"{P}:BLOCK:UPDATE:easylist:OK:bytes=1400",
    f"{P}:BLOCK:CONVERT:easylist:rules=3:dropped=1:parts=1:t=0",
    f"{P}:BLOCK:LIST:easylist-00-0:0",
    f"{P}:BLOCK:READY:easylist:rules=3",
)

BLOCKER = log(
    f"{P}:BLOCK:UP",
    f"{P}:BLOCK:LIST:easylist-00-0:0",
    f"{P}:BLOCK:READY:easylist:rules=3",
    f"{P}:LOAD:{b.PAGE_URL}",
    f"{P}:TITLE:{b.BLOCKED_TITLE}",
    f"{P}:BLOCK:COUNT:3",
    f"{P}:LOAD:{b.BLOCKER_PAGE}",
    f"{P}:BLOCK:SITE:{b.SITE_NAME}:OFF",
    f"{P}:LOAD:{b.PAGE_URL}",
    f"{P}:TITLE:{b.OPEN_TITLE}",
    f"{P}:LOAD:{b.BLOCKER_PAGE}",
    f"{P}:BLOCK:UPDATE:easylist:NOTMODIFIED",
)

PERSISTED = log(
    f"{P}:BLOCK:READY:easylist:rules=3",
    f"{P}:LOAD:{b.PAGE_URL}",
    f"{P}:TITLE:{b.OPEN_TITLE}",
)


def request(host: str, path: str, conditional: str = "", status: int = 200, cookie: str = "") -> sites.Request:
    return sites.Request("https", host, "GET", path, status, "AppleWebKit", host, cookie, conditional)


def record(*requests: sites.Request) -> sites.Record:
    return sites.Record(list(requests), [])


AD = sites.AD_HOSTS[0]
TRACKER = sites.AD_HOSTS[1]
OLDNET = "theoldnet.com"

FIRST_RECORD = record(request(sites.LIST_HOST, sites.LIST_PATH))
BLOCKER_RECORD = record(
    request(OLDNET, "/webplus/blocker.html"),
    request(AD, "/allowed.js"),
    request(OLDNET, "/images/logo.png"),
    request(OLDNET, "/webplus/blocker.html"),
    request(AD, "/banner.gif"),
    request(TRACKER, "/pixel.gif"),
    request(AD, "/ad.js"),
    request(AD, "/allowed.js"),
    request(OLDNET, "/images/logo.png"),
    request(sites.LIST_HOST, sites.LIST_PATH, sites.LIST_ETAG, 304),
)
PERSISTED_RECORD = record(
    request(OLDNET, "/webplus/blocker.html"),
    request(AD, "/banner.gif"),
    request(TRACKER, "/pixel.gif"),
    request(AD, "/ad.js"),
    request(AD, "/allowed.js"),
)


class BlockerJudgeTests(unittest.TestCase):
    def test_a_right_run_passes(self) -> None:
        self.assertEqual(b.judge_lists_first(FIRST), [])
        self.assertEqual(b.judge_blocker_boot(BLOCKER), [])
        self.assertEqual(b.judge_persisted_boot(PERSISTED), [])
        self.assertEqual(b.judge_blocker_servers(FIRST_RECORD, BLOCKER_RECORD, PERSISTED_RECORD), [])

    def test_a_list_that_never_became_ready_is_named(self) -> None:
        text = log(f"{P}:BLOCK:UPDATE:easylist:OK:bytes=1", f"{P}:BLOCK:CONVERT:easylist:rules=3:dropped=1:parts=1:t=0",
                   f"{P}:BLOCK:FAIL:easylist:compile-failed")
        problems = b.judge_lists_first(text)
        self.assertTrue(any("never became ready" in p and "compile-failed" in p for p in problems), problems)

    def test_the_list_must_be_fetched_and_converted_as_expected(self) -> None:
        problems = b.judge_lists_first(log(f"{P}:BLOCK:CONVERT:easylist:rules=9:dropped=0:parts=1:t=0",
                                           f"{P}:BLOCK:READY:easylist:rules=9"))
        self.assertTrue(any("never fetched" in p for p in problems))
        self.assertTrue(any("not converted as expected" in p for p in problems))

    def test_a_later_boot_must_not_convert_again(self) -> None:
        again = PERSISTED + f"{P}:BLOCK:CONVERT:easylist:rules=3:dropped=1:parts=1:t=0\n"
        self.assertTrue(any("converted again" in p for p in b.judge_persisted_boot(again)))
        lost = PERSISTED + f"{P}:BLOCK:RECOMPILE:easylist:not-stored\n"
        self.assertTrue(any("store did not have" in p for p in b.judge_persisted_boot(lost)))

    def test_the_blocker_boot_needs_both_titles_in_order_and_the_exception(self) -> None:
        no_block = BLOCKER.replace(f"{P}:TITLE:{b.BLOCKED_TITLE}\n", "")
        self.assertTrue(any("with the ad network blocked" in p for p in b.judge_blocker_boot(no_block)))
        no_site = BLOCKER.replace(f"{P}:BLOCK:SITE:{b.SITE_NAME}:OFF\n", "")
        self.assertTrue(any("exception" in p for p in b.judge_blocker_boot(no_site)))
        swapped = log(*[line for line in BLOCKER.splitlines()
                        if b.BLOCKED_TITLE not in line and b.OPEN_TITLE not in line],
                      f"{P}:TITLE:{b.OPEN_TITLE}", f"{P}:TITLE:{b.BLOCKED_TITLE}")
        self.assertTrue(any("whole before it was blocked" in p for p in b.judge_blocker_boot(swapped)))
        no_update = BLOCKER.replace(f"{P}:BLOCK:UPDATE:easylist:NOTMODIFIED\n", "")
        self.assertTrue(any("304" in p for p in b.judge_blocker_boot(no_update)))
        few = BLOCKER.replace(f"{P}:BLOCK:COUNT:3", f"{P}:BLOCK:COUNT:1")
        self.assertTrue(any("counted" in p for p in b.judge_blocker_boot(few)))

    def test_a_script_that_started_a_program_fails_the_boot(self) -> None:
        text = BLOCKER + f"{P}:LAUNCH:mailto:x@y:BLOCKED\n"
        self.assertTrue(any("started another program" in p for p in b.judge_blocker_boot(text)))

    def test_a_kept_exception_is_required_and_a_blocked_page_is_a_failure(self) -> None:
        blocked = log(f"{P}:BLOCK:READY:easylist:rules=3", f"{P}:TITLE:{b.BLOCKED_TITLE}")
        problems = b.judge_persisted_boot(blocked)
        self.assertTrue(any("not kept" in p for p in problems))
        self.assertTrue(any("blocked on the boot after" in p for p in problems))

    def test_servers_a_blocked_resource_asked_twice_means_it_was_not_blocked(self) -> None:
        twice = record(*BLOCKER_RECORD.requests, request(AD, "/banner.gif"))
        problems = b.judge_blocker_servers(FIRST_RECORD, twice, PERSISTED_RECORD)
        self.assertTrue(any("/banner.gif" in p and "2 times" in p for p in problems), problems)

    def test_servers_the_excepted_script_and_the_logo_load_in_both_phases(self) -> None:
        no_allowed = record(*[r for r in BLOCKER_RECORD.requests if r.path != "/allowed.js"])
        self.assertTrue(any("allowed.js" in p for p in b.judge_blocker_servers(FIRST_RECORD, no_allowed,
                                                                               PERSISTED_RECORD)))
        no_logo = record(*[r for r in BLOCKER_RECORD.requests if "logo" not in r.path])
        self.assertTrue(any("own picture" in p for p in b.judge_blocker_servers(FIRST_RECORD, no_logo,
                                                                                PERSISTED_RECORD)))

    def test_servers_the_update_is_one_conditional_request_answered_304(self) -> None:
        plain = record(*[r if r.host != sites.LIST_HOST else request(sites.LIST_HOST, sites.LIST_PATH)
                         for r in BLOCKER_RECORD.requests])
        self.assertTrue(any("Update now" in p for p in b.judge_blocker_servers(FIRST_RECORD, plain,
                                                                                PERSISTED_RECORD)))

    def test_servers_the_first_fetch_has_no_condition_and_no_cookie(self) -> None:
        conditional = record(request(sites.LIST_HOST, sites.LIST_PATH, sites.LIST_ETAG))
        self.assertTrue(any("conditional" in p for p in b.judge_blocker_servers(conditional, BLOCKER_RECORD,
                                                                                PERSISTED_RECORD)))
        cookie = record(request(sites.LIST_HOST, sites.LIST_PATH, cookie="a=b"))
        self.assertTrue(any("cookie" in p for p in b.judge_blocker_servers(cookie, BLOCKER_RECORD,
                                                                           PERSISTED_RECORD)))
        none = record()
        self.assertTrue(any("never fetched" in p for p in b.judge_blocker_servers(none, BLOCKER_RECORD,
                                                                                  PERSISTED_RECORD)))

    def test_the_sessions_are_well_formed(self) -> None:
        for steps in (b.blocker_boot(), b.persisted_boot()):
            self.assertTrue(steps and steps[-1] == {"quit": True})
            for step in steps:
                self.assertIsInstance(step, dict)
        text = str(b.blocker_boot())
        for needle in (b.PAGE_URL, "shield", b.BLOCKED_TITLE, b.OPEN_TITLE, "NOTMODIFIED"):
            self.assertIn(needle, text)


if __name__ == "__main__":
    unittest.main()
