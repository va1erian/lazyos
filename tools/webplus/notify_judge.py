"""The verdict of LazyWeb+'s notification harness (`notify_run.py`), as pure functions with their
own tests (`test_notify_judge.py`).

Three kinds of evidence, each judged on its own:

* **serial** - the window's markers (`WEBPLUS:NOTIFY:ASK|ALLOW|BLOCK|DISMISS|SHOW|CLICK|CLOSE|DROP|
  DENY|FORGET:...`, printed in a UI-probe image only: they name sites; webd hands the client a page's
  notifications and the client posts the toasts itself, so the toasts are LazyWeb+'s) and the shell's own
  (`SHELL:NOTIFY:POST owner=<app> id=<n> level=<level> title=<title>`, `SHELL:NOTIFY:SHOW id=<n>`,
  `SHELL:NOTIFY:LIMIT`). The shell's lines are the evidence of *who* the toast came from and what
  it said: a toast posted for a page is titled with the site, at level Info, by the browser;
* **page titles** - what the page learned (`notify:ready:<permission at load>:<answer without a
  gesture>`, `notify:perm:<answer>`, `notify:clicked:<title>`): the page's own view of the API;
* **profile** - `notify.tsv` read from the OS volume on the host after each boot: the decisions in
  the documented, canonical form.

The screenshots are for a person to read (the bar, the toast with the site in it).
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
sys.path.append(str(HERE.parent.parent / "tools" / "webtest"))
import judge as j  # noqa: E402
import sites  # noqa: E402

P = j.P
SITE = j.SITE
#: The two origins the harness uses: the page that is allowed, and the page that is blocked.
ORIGIN = SITE
WWW_ORIGIN = "https://www.theoldnet.com"
PAGE = SITE + sites.NOTIFY_PATH
WWW_PAGE = WWW_ORIGIN + sites.NOTIFY_PATH
#: The host a toast is titled with.
HOST = "theoldnet.com"
WWW_HOST = "www.theoldnet.com"
#: What the page's script titles itself (`fixtures/theoldnet.com/webplus/notify.html`).
READY_DEFAULT = "notify:ready:default:denied"
READY_GRANTED = "notify:ready:granted:denied"
PERM_GRANTED = "notify:perm:granted"
PERM_DENIED = "notify:perm:denied"
CLICKED_SECOND = "notify:clicked:Second title"
CLICKED_LATER = "notify:clicked:Later title"
NOTIFY_FILE = "notify.tsv"

POST = re.compile(r"SHELL:NOTIFY:POST owner=(?P<owner>\S+) id=(?P<id>\d+) level=(?P<level>\w+) title=(?P<title>[^\r\n]*)")


def notify(text: str, kind: str) -> list[str]:
    """What follows `WEBPLUS:NOTIFY:<kind>:` on every such marker."""
    return j.lines(text, f"{P}:NOTIFY:{kind}:")


def shows(text: str, origin: str) -> list[int]:
    """The ids of the `SHOW:<origin>:<id>` markers, in order."""
    found = []
    for line in notify(text, "SHOW"):
        site, _, ident = line.rpartition(":")
        if site == origin and ident.isdigit():
            found.append(int(ident))
    return found


def posts(text: str) -> list[dict]:
    """The shell's POST lines: owner, toast id, level, title."""
    return [{"owner": m["owner"], "id": int(m["id"]), "level": m["level"], "title": m["title"].strip()}
            for m in POST.finditer(text)]


def web_posts(text: str) -> list[dict]:
    """The POST lines that are the browser's: the ones whose owner is not the Terminal or the system."""
    return [p for p in posts(text) if not p["owner"].startswith("<")]


def count(text: str, kind: str, origin: str) -> int:
    return sum(1 for line in notify(text, kind) if line == origin or line.startswith(origin + ":"))


def judge_identity(text: str, allowed_hosts: tuple[str, ...]) -> list[str]:
    """Who the shell saw: every toast of the browser is titled with a site that was allowed, at level
    Info, by one app; never the page's own words, never another level."""
    problems = []
    web = web_posts(text)
    if not web:
        return [f"the shell saw no toast from the browser (SHELL:NOTIFY:POST lines: {posts(text)})"]
    owners = {p["owner"] for p in web}
    if len(owners) != 1:
        problems.append(f"toasts came from {len(owners)} different owners: {sorted(owners)}")
    for owner in sorted(owners):
        if re.search(r"webd|web engine", owner, re.I):
            problems.append(f"a toast was posted by the web engine daemon ({owner}), not by the browser")
    for p in web:
        if p["title"] not in allowed_hosts:
            problems.append(f"a toast was titled {p['title']!r}, expected the site (one of {allowed_hosts})")
        if p["level"] != "info":
            problems.append(f"a toast of the page was posted at level {p['level']}, expected info")
    if "SHELL:NOTIFY:LIMIT" in text:
        problems.append("the shell's own rate limit was hit: the browser's per-site limits did not hold")
    return problems


def judge_allow(text: str) -> list[str]:
    """The first boot: no gesture, no bar; a click asks; Allow is kept; a post is a toast of the
    site; a tag updates it in place; a burst is cut short; a click reaches the page, and a click
    on a toast of a page that has gone brings the page back."""
    problems = judge_boot(text)
    seen = j.titles(text)
    for title, why in ((READY_DEFAULT, "the page's request without a gesture was not refused"),
                       (PERM_GRANTED, "the page did not learn it was allowed"),
                       (CLICKED_SECOND, "the click did not reach the page's handler"),
                       (CLICKED_LATER, "the click on a toast posted while the window was minimized did not "
                                       "reach the page's handler")):
        if title not in seen:
            problems.append(f"{why}: no {P}:TITLE:{title} (seen: {seen})")
    if count(text, "ASK", ORIGIN) != 1:
        problems.append(f"expected exactly one {P}:NOTIFY:ASK:{ORIGIN} (the load-time request has no gesture), "
                        f"got {count(text, 'ASK', ORIGIN)}")
    if count(text, "ALLOW", ORIGIN) != 1:
        problems.append(f"no {P}:NOTIFY:ALLOW:{ORIGIN}")
    shown = shows(text, ORIGIN)
    if len(shown) < 2:
        problems.append(f"expected a toast and its update ({P}:NOTIFY:SHOW:{ORIGIN}:<id> twice), got {shown}")
    web = web_posts(text)
    if len(web) >= 2:
        if web[0]["id"] != web[1]["id"]:
            problems.append(f"the tag's update was a second toast: ids {web[0]['id']} and {web[1]['id']}")
        elif len(re.findall(rf"SHELL:NOTIFY:SHOW id={web[0]['id']}\b", text)) != 1:
            problems.append(f"toast {web[0]['id']} was shown more than once")
    else:
        problems.append(f"the shell saw {len(web)} post(s) from the browser, expected the toast and its update")
    drops = count(text, "DROP", ORIGIN)
    if drops < 3:
        problems.append(f"the burst of six was not cut: {drops} {P}:NOTIFY:DROP lines, expected 3 or more")
    # The toast and its update, the one of the page that left, the one posted while minimized, and
    # at most the site's three live ones from the burst.
    if len(web) > 4 + 3:
        problems.append(f"the burst put {len(web) - 4} toasts up, the site limit is 3")
    clicks = notify(text, "CLICK")
    if len(clicks) < 3:
        problems.append(f"expected three clicks ({P}:NOTIFY:CLICK:<id>), got {clicks}")
    if "XUID:WM:MINIMIZE" not in text:
        problems.append("the window was never minimized: no XUID:WM:MINIMIZE")
    elif text.find("XUID:WM:UNMINIMIZE") < text.find("XUID:WM:MINIMIZE"):
        problems.append("a click on a toast did not bring the minimized window back: no XUID:WM:UNMINIMIZE "
                        "after XUID:WM:MINIMIZE")
    if sum(1 for url in j.markers(text, "LOAD") if url.rstrip("/") == PAGE) < 2:
        problems.append(f"the click on a toast of a page that had gone did not bring it back: "
                        f"{P}:LOAD:{PAGE} fewer than twice")
    problems += judge_identity(text, (HOST,))
    return problems


def judge_kept(text: str) -> list[str]:
    """The second boot: the allowed site notifies with no bar, the blocked one shows nothing, and the
    list can be changed from `about:lazyweb`."""
    problems = judge_boot(text)
    seen = j.titles(text)
    for title, why in ((READY_GRANTED, "the allowed site was not remembered"),
                       (PERM_DENIED, "the site that was blocked did not learn it")):
        if title not in seen:
            problems.append(f"{why}: no {P}:TITLE:{title} (seen: {seen})")
    if count(text, "ASK", ORIGIN):
        problems.append(f"the allowed site was asked about again: {P}:NOTIFY:ASK:{ORIGIN}")
    if not shows(text, ORIGIN):
        problems.append(f"the allowed site showed nothing: no {P}:NOTIFY:SHOW:{ORIGIN}:<id>")
    if count(text, "ASK", WWW_ORIGIN) != 1:
        problems.append(f"expected one {P}:NOTIFY:ASK:{WWW_ORIGIN}, got {count(text, 'ASK', WWW_ORIGIN)}")
    if count(text, "BLOCK", WWW_ORIGIN) != 1:
        problems.append(f"no {P}:NOTIFY:BLOCK:{WWW_ORIGIN}")
    if shows(text, WWW_ORIGIN):
        problems.append(f"the blocked site showed a toast: {shows(text, WWW_ORIGIN)}")
    if any(p["title"] == WWW_HOST for p in posts(text)):
        problems.append(f"the shell saw a toast for the blocked site {WWW_HOST}")
    if not j.loaded(text, "about:lazyweb"):
        problems.append(f"no {P}:LOAD:about:lazyweb")
    if not any(line == WWW_ORIGIN for line in notify(text, "FORGET")):
        problems.append(f"the decision was not forgotten from about:lazyweb: no {P}:NOTIFY:FORGET:{WWW_ORIGIN}")
    problems += judge_identity(text, (HOST,))
    return problems


def judge_boot(text: str) -> list[str]:
    """What every boot owes: the browser and its children came up, and nothing failed or was started
    on a page's say."""
    problems = j.judge_start(text)
    for line in notify(text, "DENY"):
        # Refusals are the point of the harness, but these two mean a bug, not a refusal of a page.
        if line.endswith((":origin", ":shell")):
            problems.append(f"a notification was refused for a bug's reason: {P}:NOTIFY:DENY:{line}")
    for line in j.markers(text, "LAUNCH"):
        problems.append(f"something started another program: {P}:LAUNCH:{line}")
    return problems


def tsv_rows(tsv: str) -> list[tuple[str, str, str]]:
    """The (origin, decision, time) rows of a `notify.tsv`."""
    rows = []
    for line in tsv.splitlines():
        fields = line.split("\t")
        if len(fields) == 3:
            rows.append((fields[0], fields[1], fields[2]))
    return rows


def judge_profile(after_first: str | None, after_second: str | None) -> list[str]:
    """`notify.tsv` on the OS volume: after the first boot it holds the allowed site, in the file's
    documented form; after the second, the blocked site was forgotten again and the allowed one stays."""
    problems = []
    if after_first is None:
        return ["the profile has no notify.tsv after the first boot: the decision was not kept"]
    rows = tsv_rows(after_first)
    if [(o, d) for o, d, _ in rows] != [(ORIGIN, "allow")]:
        problems.append(f"notify.tsv after the first boot holds {rows}, expected only {ORIGIN} allow")
    for origin, decision, when in rows:
        if not when.isdigit():
            problems.append(f"notify.tsv time is not seconds since the epoch: {when!r}")
    if after_second is None:
        problems.append("the profile has no notify.tsv after the second boot")
    else:
        kept = [(o, d) for o, d, _ in tsv_rows(after_second)]
        if kept != [(ORIGIN, "allow")]:
            problems.append(f"notify.tsv after the second boot holds {kept}, expected only {ORIGIN} allow "
                            f"(the blocked site was forgotten from about:lazyweb)")
    return problems


def judge_servers(first, second) -> list[str]:
    """The host's stand-in sites saw the page, from both origins, over HTTPS."""
    problems = []
    for name, record, hosts in (("first", first, (HOST,)), ("second", second, (HOST, WWW_HOST))):
        for host in hosts:
            if not any(r.scheme == "https" and r.host == host and r.path == sites.NOTIFY_PATH
                       and r.status == 200 for r in record.requests):
                problems.append(f"the {name} boot never fetched https://{host}/webplus/notify.html")
    return problems
