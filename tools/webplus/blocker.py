"""The ad blocker's part of LazyWeb+'s harness (issue #845, docs/lazywebplus.md).

The image is the same one the other boots use. Its browser fetches the filter list from
`easylist.to` (the harness serves a small list of its own there, `tools/web/sites.py`) at its
first start, so the **first boot** has the list fetched, converted and compiled in the
background. Two more boots of the image:

* **blocker boot** (`blocker`): the list comes from WebKit's store (no conversion), a page with an
  ad picture, a tracker pixel, two scripts and a picture of its own loads with the ad network
  blocked and the rest not; the user opens `about:blocker` (the toolbar's shield), turns blocking
  off for the site, and the same page loads whole; "Update now" asks the list's host again, with
  its ETag, and is told nothing changed;
* **persisted boot** (`persisted`): the site's exception was kept (a `confd` key), so the page
  loads whole at once, and the list again comes from the store.

What the host's servers saw is the evidence: a blocked resource is never requested, so the
request log shows each ad-network URL exactly once, in the part of the run where blocking was off.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import judge as j  # noqa: E402

sites = j.sites

LIST_ID = "easylist"
PAGE_URL = j.SITE + "/webplus/blocker.html"
BLOCKED_TITLE = "blocker:ad=blocked,tracker=blocked,script=blocked,allowed=loaded,logo=loaded"
OPEN_TITLE = "blocker:ad=loaded,tracker=loaded,script=loaded,allowed=loaded,logo=loaded"
BLOCKER_PAGE = "about:blocker"
SITE_NAME = "theoldnet.com"
#: The harness list's own counts (`sites.easylist_text`): two blocks and an exception, one hidden element.
LIST_RULES = 3
LIST_DROPPED = 1
#: The page's ad-network resources, by path, and the one the list lets through.
BLOCKED_PATHS = ("/banner.gif", "/pixel.gif", "/ad.js")
ALLOWED_PATH = sites.AD_ALLOWED_PATH
#: Where the pointer rests to give the blocker page the keyboard: a corner of the page, no link.
FOCUS_AT = (700, 380)


# ---- the serial log -----------------------------------------------------------------------


def block_markers(text: str, kind: str) -> list[str]:
    """What follows `WEBPLUS:BLOCK:<kind>:` on every such line."""
    return j.markers(text, f"BLOCK:{kind}")


def judge_lists_first(text: str) -> list[str]:
    """The first boot found no list: it fetched, converted and compiled it, and it became ready."""
    problems = []
    if not any(m.startswith(f"{LIST_ID}:OK") for m in block_markers(text, "UPDATE")):
        problems.append(f"the list was never fetched ({j.P}:BLOCK:UPDATE:{LIST_ID}:OK; saw "
                        f"{block_markers(text, 'UPDATE')})")
    converted = [m for m in block_markers(text, "CONVERT") if m.startswith(f"{LIST_ID}:")]
    want = f"{LIST_ID}:rules={LIST_RULES}:dropped={LIST_DROPPED}:parts=1"
    if not any(m.startswith(want) for m in converted):
        problems.append(f"the list was not converted as expected ({want}...; saw {converted})")
    problems += judge_ready(text)
    return problems


def judge_ready(text: str) -> list[str]:
    ready = [m for m in block_markers(text, "READY") if m.startswith(f"{LIST_ID}:")]
    if not ready:
        failures = block_markers(text, "FAIL") + block_markers(text, "UNAVAILABLE")
        return [f"the list never became ready ({j.P}:BLOCK:READY:{LIST_ID}); saw {failures or 'nothing'}"]
    return []


def judge_from_store(text: str) -> list[str]:
    """A later boot: the compiled list is in WebKit's store, so nothing is converted again."""
    problems = judge_ready(text)
    if block_markers(text, "CONVERT"):
        problems.append(f"the list was converted again on a later start: {block_markers(text, 'CONVERT')}")
    if block_markers(text, "RECOMPILE"):
        problems.append(f"the store did not have the list: {block_markers(text, 'RECOMPILE')}")
    return problems


def judge_blocker_boot(text: str) -> list[str]:
    """The blocker boot's own markers: blocked, then the site excepted, then the page whole, then
    the list asked after and told it had not changed."""
    problems = judge_from_store(text)
    seen = j.titles(text)
    for title, why in ((BLOCKED_TITLE, "with the ad network blocked"), (OPEN_TITLE, "whole, once the site was excepted")):
        if title not in seen:
            problems.append(f"the page never loaded {why}: no {j.P}:TITLE:{title} (titles: {seen})")
    if BLOCKED_TITLE in seen and OPEN_TITLE in seen and seen.index(BLOCKED_TITLE) > seen.index(OPEN_TITLE):
        problems.append("the page was whole before it was blocked")
    if not j.loaded(text, BLOCKER_PAGE):
        problems.append(f"no {j.P}:LOAD:{BLOCKER_PAGE}")
    if f"{SITE_NAME}:OFF" not in block_markers(text, "SITE"):
        problems.append(f"the exception for {SITE_NAME} was not made (no {j.P}:BLOCK:SITE:{SITE_NAME}:OFF)")
    counts = [int(m) for m in block_markers(text, "COUNT") if m.isdigit()]
    if not counts or max(counts) < 3:
        problems.append(f"the engine counted {counts} blocked loads on the page (wanted 3: banner, pixel, script)")
    if f"{LIST_ID}:NOTMODIFIED" not in block_markers(text, "UPDATE"):
        problems.append(f"Update now did not get a 304 ({j.P}:BLOCK:UPDATE:{LIST_ID}:NOTMODIFIED; saw "
                        f"{block_markers(text, 'UPDATE')})")
    for line in j.markers(text, "LAUNCH"):
        problems.append(f"something started another program: {j.P}:LAUNCH:{line}")
    return problems


def judge_persisted_boot(text: str) -> list[str]:
    """The persisted boot: the exception outlived the boot, the page is whole at once."""
    problems = judge_from_store(text)
    seen = j.titles(text)
    if OPEN_TITLE not in seen:
        problems.append(f"the exception was not kept: no {j.P}:TITLE:{OPEN_TITLE} (titles: {seen})")
    if BLOCKED_TITLE in seen:
        problems.append("the page was blocked on the boot after the user excepted its site")
    return problems


# ---- the host's request log ---------------------------------------------------------------


def ad_requests(requests, path: str | None = None) -> list:
    """The requests that reached the stand-in ad network."""
    return [r for r in requests if r.host in sites.AD_HOSTS and (path is None or r.path.split("?")[0] == path)]


def judge_blocker_servers(first, blocker, persisted) -> list[str]:
    """First boot: the list was fetched (plain, no condition). Blocker boot: every blocked resource
    was asked for exactly once, after the exception; the allowed script twice (blocked and whole);
    the page's own picture both times; the list host asked once more, conditionally. Persisted
    boot: the page whole, nothing blocked."""
    problems = []
    fetched = [r for r in first.requests if r.host == sites.LIST_HOST and r.path.split("?")[0] == sites.LIST_PATH]
    if not fetched:
        problems.append(f"the first boot never fetched {sites.LIST_HOST}{sites.LIST_PATH}")
    elif fetched[0].conditional:
        problems.append(f"the first fetch was conditional ({fetched[0].conditional!r}) with nothing stored")
    elif fetched[0].cookie:
        problems.append(f"the list fetch carried a cookie: {fetched[0].cookie!r}")
    if first.requests and any(r.sni != r.host for r in fetched if r.scheme == "https"):
        problems.append("the list fetch's SNI is not its host")

    for path in BLOCKED_PATHS:
        count = len(ad_requests(blocker.requests, path))
        if count != 1:
            problems.append(f"the blocker boot asked the ad network for {path} {count} times (wanted 1: "
                            "blocked while blocking was on, loaded after the exception)")
    allowed = len(ad_requests(blocker.requests, ALLOWED_PATH))
    if allowed != 2:
        problems.append(f"the excepted script {ALLOWED_PATH} was requested {allowed} times (wanted 2)")
    logos = [r for r in blocker.requests if r.host in j.OLDNET_HOSTS and r.path.endswith("/images/logo.png")]
    if len(logos) < 2:
        problems.append(f"the page's own picture was requested {len(logos)} times (wanted both loads)")
    updates = [r for r in blocker.requests if r.host == sites.LIST_HOST and r.path.split("?")[0] == sites.LIST_PATH]
    if len(updates) != 1 or sites.LIST_ETAG not in updates[0].conditional or updates[0].status != 304:
        problems.append("Update now was not one conditional request answered 304: "
                        f"{[(r.conditional, r.status) for r in updates]}")

    if len(ad_requests(persisted.requests, ALLOWED_PATH)) != 1 or any(
            len(ad_requests(persisted.requests, p)) != 1 for p in BLOCKED_PATHS):
        problems.append("the persisted boot did not load the page whole, once: "
                        f"{[r.path for r in ad_requests(persisted.requests)]}")
    return problems


# ---- the sessions -------------------------------------------------------------------------


def _wait(marker: str, timeout: float) -> dict:
    return {"wait_for": marker, "timeout": timeout}


def blocker_boot(timeout: float = 300.0) -> list[dict]:
    """The blocker boot; see the module note. Keys, not coordinates, move through `about:blocker`:
    a click on the empty page gives it the keyboard, Tab walks its links in order, Enter follows."""
    import session as s
    loaded = lambda url: f"{j.P}:LOAD:{url}"  # noqa: E731
    titled = lambda title: f"{j.P}:TITLE:{title}"  # noqa: E731
    blocked_window = j.caption(BLOCKED_TITLE)
    open_window = j.caption(OPEN_TITLE)
    blocker_window = j.caption("Ad blocker")
    return [
        *s.boot_gates(timeout),
        _wait(loaded(j.START), timeout),
        _wait(f"{j.P}:BLOCK:READY:{LIST_ID}", timeout),
        # The page, with the ad network blocked.
        *s.navigate(PAGE_URL, loaded(PAGE_URL), timeout),
        _wait(titled(BLOCKED_TITLE), timeout),
        {"wait": 2.0},
        {"shot": "40_blocked"},
        # The shield: about:blocker, with this site on it.
        {"click_at": {"window": blocked_window, "widget": "shield"}, "timeout": timeout,
         "until": loaded(BLOCKER_PAGE), "retries": 2},
        {"wait": 3.0},
        {"shot": "41_blocker_page"},
        # Link 2 is "Turn off for <site>": click the page, Tab twice, Enter.
        s.page_click(blocker_window, FOCUS_AT, timeout),
        {"key": "tab"}, {"key": "tab"},
        {"key": "enter", "until": f"{j.P}:BLOCK:SITE:{SITE_NAME}:OFF", "timeout": timeout},
        {"wait": 3.0},
        {"shot": "42_site_off"},
        # Link 3 is now "Go back to <site>" (the sentence has one button): the page, whole.
        s.page_click(blocker_window, FOCUS_AT, timeout),
        {"key": "tab"}, {"key": "tab"}, {"key": "tab"},
        {"key": "enter", "until": loaded(PAGE_URL), "timeout": timeout},
        _wait(titled(OPEN_TITLE), timeout),
        {"wait": 2.0},
        {"shot": "43_whole"},
        # The shield again, then "Update now" (link 5 with the site excepted).
        {"click_at": {"window": open_window, "widget": "shield"}, "timeout": timeout,
         "until": loaded(BLOCKER_PAGE), "retries": 2},
        {"wait": 3.0},
        s.page_click(blocker_window, FOCUS_AT, timeout),
        *[{"key": "tab"} for _ in range(5)],
        {"key": "enter", "until": f"{j.P}:BLOCK:UPDATE:{LIST_ID}:NOTMODIFIED", "timeout": timeout},
        {"wait": 2.0},
        {"shot": "44_updated"},
        {"click_at": {"target": f"title:{blocker_window}", "offset": list(s.CLOSE_AT_NORMAL)},
         "timeout": 60, "until": f"{j.P}:QUIT:", "retries": 2},
        {"wait": 9.0},
        {"quit": True},
    ]


def persisted_boot(timeout: float = 300.0) -> list[dict]:
    """The boot after: the exception was kept, the page is whole, the list is from the store."""
    import session as s
    loaded = lambda url: f"{j.P}:LOAD:{url}"  # noqa: E731
    return [
        *s.boot_gates(timeout),
        _wait(loaded(j.START), timeout),
        _wait(f"{j.P}:BLOCK:READY:{LIST_ID}", timeout),
        *s.navigate(PAGE_URL, loaded(PAGE_URL), timeout),
        _wait(f"{j.P}:TITLE:{OPEN_TITLE}", timeout),
        {"wait": 2.0},
        {"shot": "50_kept"},
        {"click_at": {"target": f"title:{j.caption(OPEN_TITLE)}", "offset": list(s.CLOSE_AT_NORMAL)},
         "timeout": 60, "until": f"{j.P}:QUIT:", "retries": 2},
        {"wait": 9.0},
        {"quit": True},
    ]


def title_pattern() -> re.Pattern[str]:
    """Any of the blocker page's titles (for a judge that does not care which)."""
    return re.compile(r"blocker:ad=\w+,tracker=\w+,script=\w+,allowed=\w+,logo=\w+")
