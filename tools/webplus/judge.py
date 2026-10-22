"""LazyWeb+'s harness verdict (`run.py`), as pure functions with their own tests (`test_judge.py`).

Four kinds of evidence, each judged on its own:

* **serial** - the window's markers (the client's, `WEBPLUS:UP:PASS`, `LOAD:<url>`, `TITLE:<title>`,
  `NAV`, `LAUNCH:<url>:OK|FAIL|BLOCKED`, `POPUP:<url>`, `DIALOG:<kind>`, `DOWNLOAD:START|DONE|FAIL`,
  `FAIL`, `STOPPED`, `QUIT:PASS`) and the engine's, which `webd` relays as `WEBD:NOTE:<line>`
  (`engine_lines`: `WEBPROCESS:UP:<pid>`, `NETWORKPROCESS:UP:<pid>`, `PROCESS:LAUNCH|EXIT|TERMINATE`,
  `INFO:*`) beside its own `WEBD:VIEW:CREATE|DESTROY` and `WEBD:LOST`: every page loaded with its
  title, the script's three dialogs were answered (the title carries the answers), what the script
  tried on its own was `BLOCKED` and nothing but the user's click started another program, the
  downloads finished whole;
* **servers** - what the host's stand-in sites recorded (`tools/webtest/sites.py`): the pages, every
  picture, the form's query, no cookie on the first boot's request and the first boot's cookie on
  the second's (the profile is persistent);
* **profile** - read from the OS volume on the host after each boot: the history file holds the
  pages of the first boot, the cookie database exists, the downloads are in the Downloads folder
  whole, byte for byte, with no `.part` left, and the second boot's Clear history emptied the file;
* **screenshots** - the page area of each shot is not blank, the pages differ, the wheel moved the
  static bands, the maximized page is painted to its right edge.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.append(str(ROOT / "tools" / "webtest"))
sys.path.append(str(ROOT / "tools" / "screenshot"))
sys.path.append(str(ROOT / "tools"))
sys.path.insert(0, str(HERE))
import serial_integrity  # noqa: E402  (a log with SERIAL:DROPPED proves nothing)
import session_pointer  # noqa: E402
import pages  # noqa: E402  (the shared fixtures' pages: tools/webtest)
import sites  # noqa: E402

from pixels import SCROLL_PX, judge_maximized, judge_scroll, region_stats  # noqa: E402,F401  (the runner's)

#: The client's marker prefix.
P = "WEBPLUS"
#: The prefix of an engine marker, which `webd` relays (`xui-app/webd/src/lines.rs`): what follows it is
#: the line the engine's C++ half prints (`WEBPROCESS:UP:<pid>`, `INFO:PAINT:...`). Read through
#: `engine_lines`, never by a literal `WEBPLUS:` prefix.
NOTE = "WEBD:NOTE:"
#: webd's own markers.
WEBD_UP = "WEBD:UP:PASS"
#: The package label of the client, as webd names the caller of a view.
CLIENT_LABEL = "app:os.lazy.lazywebplus"
VIEW_CREATE = re.compile(r"WEBD:VIEW:CREATE client=(?P<client>\S+) view=(?P<view>\d+)")
VIEW_DESTROY = re.compile(r"WEBD:VIEW:DESTROY view=(?P<view>\d+)")
VIEW_LOST = re.compile(r"WEBD:LOST view=(?P<view>\d+) reason=(?P<reason>\w+)")
APP = "LazyWeb+"
START = "about:start"
EXAMPLE_URL = pages.EXAMPLE_URL
EXAMPLE_TITLE = pages.EXAMPLE_TITLE
OLDNET_URL = pages.OLDNET_URL
OLDNET_TITLE = pages.OLDNET_TITLE
OLDNET_HOSTS = pages.OLDNET_HOSTS
SITE = "https://theoldnet.com"
COOKIE_URL = SITE + sites.COOKIE_PATH
COOKIE_NONE = "cookie:none"
COOKIE_KEPT = f"cookie:{sites.COOKIE_VALUE}"
SCRIPT_URL = SITE + "/webplus/script.html"
PROMPT_TEXT = "lazy"
#: What the page's script titles itself after its three dialogs: the answers reached the page.
SCRIPT_TITLE = f"script-ok:yes:{PROMPT_TEXT}"
EVIL_MAILTO = "mailto:evil@theoldnet.com?subject=script"
COMMAND_URL = "x-lazyweb:clear-history"
LINKS_URL = SITE + "/webplus/links.html"
LINKS_TITLE = "WebPlus links"
MAIL_URL = "mailto:lazy@theoldnet.com?subject=hello"
BLANK_URL = SITE + "/webplus/blank-target.html"
BLANK_TITLE = "opened:blank"
OPENED_URL = SITE + "/webplus/opened.html"
OPENED_TITLE = "opened:script"
DOWNLOAD_NAME = sites.DOWNLOAD_NAME
DOWNLOAD_BYTES = len(sites.download_payload())
SLOW_NAME = sites.SLOW_DOWNLOAD_NAME
SLOW_BYTES = len(sites.slow_download_payload())
TALL_URL = SITE + "/webkit/tall.html"
TALL_TITLE = "WebKit tall"
#: The links page (`fixtures/theoldnet.com/webkit/`): its red box is a link
#: to the form, whose field takes the typed text and whose submit goes to the echo.
LINKS_FORM_URL = SITE + "/webkit/links.html"
LINKS_FORM_TITLE = "WebKit links"
FORM_URL = SITE + "/webkit/form.html"
FORM_TITLE = "WebKit form"
#: What the session types into the form; the echo page's title is `echo:` + it.
INPUT_TEXT = "hello lazyos"
ECHO_TITLE = f"echo:{INPUT_TEXT}"
#: A page that never answers in time (`sites.SLOW_PATH`): Stop ends it, and that is no failure.
SLOW_URL = SITE + sites.SLOW_PATH
#: The host accepts a connection here and hangs up (`sites.Closer`): the load fails at once and the
#: window says so.
CLOSER_PORT = 81
FAIL_URL = f"http://example.com:{CLOSER_PORT}/"
page_assets = pages.page_assets
HISTORY_FILE = "history.tsv"
#: webd's `browser` profile, below the home: the engine's cookie jar, HTTP cache and rule lists live
#: there, not in LazyWeb+'s own folder (which keeps `history.tsv`, `notify.tsv` and the list sources).
WEBD_PROFILE = ".apps/os.lazy.webd/profiles/browser"
#: Where the engine keeps the cookie jar below that folder.
COOKIE_DB = "cookies/cookie.jar.db"
#: The cookie jar's name in `run.read_profile`'s listing (a name under the home).
COOKIE_KEY = f"{WEBD_PROFILE}/{COOKIE_DB}"


def caption(title: str) -> str:
    """The window's caption while a page with `title` is on show (`app.rs` `caption`)."""
    return f"{title} - {APP}"


def lines(text: str, prefix: str) -> list[str]:
    """What follows `prefix` on every line that has it (anywhere: the console may interleave)."""
    found = []
    for line in text.splitlines():
        at = line.find(prefix)
        if at >= 0:
            found.append(line[at + len(prefix):].strip())
    return found


def markers(text: str, kind: str) -> list[str]:
    """The text after `WEBPLUS:<kind>:` of every such marker."""
    return lines(text, f"{P}:{kind}:")


def engine_lines(text: str) -> list[str]:
    """The engine's markers of a log, in order, without webd's `WEBD:NOTE:` prefix: webd relays what the
    WebKit half prints (`PROCESS:LAUNCH:web:41`, `WEBPROCESS:UP:41`, `INFO:PAINT:...`). Every reader of an
    engine marker goes through here; the client's own markers stay `markers(text, ...)`."""
    return lines(text, NOTE)


def engine_text(text: str) -> str:
    """`engine_lines` as one text, a marker a line, for the regular expressions that read several."""
    return "".join(f"{line}\n" for line in engine_lines(text))


def engine_markers(text: str, kind: str) -> list[str]:
    """The text after `<kind>:` of every engine marker that starts with it."""
    return [line[len(kind) + 1:] for line in engine_lines(text) if line.startswith(f"{kind}:")]


def engine_pids(text: str, kind: str, what: str) -> list[str]:
    """The pids of every engine `<kind>:<what>:<pid>...` marker, in order."""
    return re.findall(rf"^{kind}:{what}:(\d+)", engine_text(text), re.M)


def loaded(text: str, url: str) -> bool:
    return any(seen.rstrip("/") == url.rstrip("/") for seen in markers(text, "LOAD"))


def titles(text: str) -> list[str]:
    return markers(text, "TITLE")


def judge_start(text: str) -> list[str]:
    """The window came up (its first frame), webd made its view, and the engine started both processes
    of the page."""
    problems = []
    if f"{P}:UP:PASS" not in text:
        problems.append(f"no {P}:UP:PASS")
    if WEBD_UP not in text:
        problems.append(f"no {WEBD_UP}: the web engine daemon never came up")
    if not any(m["client"] == CLIENT_LABEL for m in VIEW_CREATE.finditer(text)):
        problems.append(f"webd never made the window's view: no WEBD:VIEW:CREATE client={CLIENT_LABEL}")
    for kind in ("WEBPROCESS", "NETWORKPROCESS"):
        if not engine_pids(text, kind, "UP"):
            problems.append(f"no {NOTE}{kind}:UP:<pid>")
    for failure in re.findall(rf"{P}:(?:BIND|RUN):FAIL[^\r\n]*", text):
        problems.append(f"the window failed: {failure}")
    problems += judge_no_fault(text) + judge_no_lost(text)
    return problems


def judge_no_lost(text: str) -> list[str]:
    """webd never reported a view lost: outside the hung-page harness, nothing takes a page's process
    away (a crash, memory, or a Stop), and a lost view blanks the page."""
    return [f"webd lost a view: {m.group(0)}" for m in VIEW_LOST.finditer(text)][:8]


def judge_no_fault(text: str) -> list[str]:
    """No task of the run died of a CPU fault: the kernel prints `user: task N killed by <fault> ...` for
    each (a trap in a child, say, which the page only shows as an "internal error" load failure)."""
    return serial_integrity.problems(text) + [f"a task died of a fault: {line}" for line in
            re.findall(r"user: task \d+ killed by [^\r\n]*", text)[:8]]


def expected_failure(failure: str) -> bool:
    """The one load the session makes fail on purpose (`FAIL_URL`): its marker names it in a probe image."""
    return failure.startswith(f"Could not load {FAIL_URL}") or failure.startswith(f"provisional:{FAIL_URL}:")


def load_failures(text: str) -> list[str]:
    """What follows `FAIL` in the client's `WEBPLUS:FAIL[:<error>]` and in the engine's own
    `FAIL:<what>` markers (`FAIL:provisional:<url>:...`): the text starts with `:` when there is one."""
    return lines(text, f"{P}:FAIL") + [line[len("FAIL"):] for line in engine_lines(text) if line.startswith("FAIL")]


def judge_pages(text: str) -> list[str]:
    """Every page of the first boot loaded with its title, and the failures are the planned one."""
    problems = []
    seen = titles(text)
    for url, title in ((START, "LazyWeb+"), (EXAMPLE_URL, EXAMPLE_TITLE), (OLDNET_URL, OLDNET_TITLE),
                       (COOKIE_URL, COOKIE_NONE), (SCRIPT_URL, SCRIPT_TITLE), (LINKS_URL, LINKS_TITLE),
                       (BLANK_URL, BLANK_TITLE), (OPENED_URL, OPENED_TITLE), (TALL_URL, TALL_TITLE),
                       (LINKS_FORM_URL, LINKS_FORM_TITLE), (FORM_URL, FORM_TITLE), ("about:history", "History"),
                       ("about:downloads", "Downloads")):
        if not loaded(text, url):
            problems.append(f"no {P}:LOAD:{url}")
        if title not in seen:
            problems.append(f"no {P}:TITLE:{title} (titles seen: {seen})")
    if ECHO_TITLE not in seen:
        problems.append(f"the typed text did not come back: no {P}:TITLE:{ECHO_TITLE} (seen: {seen})")
    failures = load_failures(text)
    for failure in failures:
        if failure and not expected_failure(failure.lstrip(":")):
            problems.append(f"a load failed: {failure}")
    if not any(expected_failure(f.lstrip(":")) for f in failures):
        problems.append(f"the address that hangs up never failed: no {P}:FAIL for {FAIL_URL}")
    if loaded(text, SLOW_URL):
        problems.append("the stopped page loaded anyway")
    if f"{P}:STOPPED" not in text:
        problems.append(f"the stopped load was not reported as stopped: no {P}:STOPPED")
    return problems


def judge_dialogs(text: str) -> list[str]:
    """The page's three dialogs reached the window and the answers reached the page (its title)."""
    problems = []
    for kind in ("alert", "confirm", "prompt"):
        if f"{P}:DIALOG:{kind}" not in text:
            problems.append(f"no {P}:DIALOG:{kind}")
    order = [m.group(1) for m in re.finditer(rf"{P}:DIALOG:(\w+)", text)]
    if order[:3] != ["alert", "confirm", "prompt"]:
        problems.append(f"the dialogs came in the order {order}, expected alert, confirm, prompt")
    if SCRIPT_TITLE not in titles(text):
        problems.append(f"the answers did not reach the page: no {P}:TITLE:{SCRIPT_TITLE}")
    return problems


def judge_launches(text: str) -> list[str]:
    """A script can start nothing on its own, a click can: the script's `mailto:` and `x-lazyweb:` are
    BLOCKED and never OK or FAIL (which would mean another program was asked), the click's `mailto:`
    reached `mimed` (OK, or FAIL when no app has the scheme: the stand-in image has no Mail)."""
    problems = []
    states: dict[str, list[str]] = {}
    for marker in markers(text, "LAUNCH"):
        url, _, state = marker.rpartition(":")
        states.setdefault(url, []).append(state)
    for url in (EVIL_MAILTO, COMMAND_URL):
        got = states.get(url, [])
        if "BLOCKED" not in got:
            problems.append(f"{url} by a script was not refused: no {P}:LAUNCH:{url}:BLOCKED (seen: {got})")
        if any(state != "BLOCKED" for state in got):
            problems.append(f"{url} by a script started something: {got}")
    clicked = states.get(MAIL_URL, [])
    if not any(state in ("OK", "FAIL") for state in clicked):
        problems.append(f"the click on the mailto: link did not reach mimed: {P}:LAUNCH:{MAIL_URL} (seen: {clicked})")
    if "BLOCKED" in clicked:
        problems.append("the click on the mailto: link was refused as if a script had made it: its user "
                        "gesture did not reach the navigation policy")
    return problems


def judge_popups(text: str) -> list[str]:
    """`target=_blank` and `window.open` from a click opened their pages in the one view; the
    script's own `window.open` (no click) opened nothing."""
    problems = []
    popups = markers(text, "POPUP")
    for url in (BLANK_URL, OPENED_URL):
        if url not in popups:
            problems.append(f"no {P}:POPUP:{url} (seen: {popups})")
    # The click's window.open comes after the links page; the script page's, which has no click,
    # must not have produced one before it.
    first_opened = next((i for i, m in enumerate(markers(text, "POPUP")) if m == OPENED_URL), None)
    if first_opened is not None and markers(text, "POPUP").count(OPENED_URL) > 1:
        problems.append("a window.open without a click opened a window")
    return problems


def judge_downloads(text: str) -> list[str]:
    problems = []
    done = markers(text, "DOWNLOAD:DONE")
    for name, size in ((DOWNLOAD_NAME, DOWNLOAD_BYTES), (SLOW_NAME, SLOW_BYTES)):
        if f"{name}:{size}" not in done:
            problems.append(f"no {P}:DOWNLOAD:DONE:{name}:{size} (seen: {done})")
        if name not in markers(text, "DOWNLOAD:START"):
            problems.append(f"no {P}:DOWNLOAD:START:{name}")
    for failure in markers(text, "DOWNLOAD:FAIL"):
        problems.append(f"a download failed: {failure}")
    return problems


def resizes(text: str) -> list[tuple[int, int]]:
    return [(int(w), int(h)) for w, h in re.findall(rf"{P}:INFO:RESIZE:(\d+)x(\d+)", text)]


def judge_resize(text: str) -> list[str]:
    sizes = resizes(text)
    if len(sizes) < 2:
        return [f"{len(sizes)} whole picture(s) of a new size arrived ({P}:INFO:RESIZE), expected the window's "
                "and the maximized one"]
    (w0, h0), (w1, h1) = sizes[0], sizes[-1]
    if w1 <= w0 or h1 <= h0:
        return [f"the page was not laid out larger after the window grew: {sizes}"]
    return []


PROCESS_EVENT = re.compile(
    r"^(?:PROCESS:(?P<kind>LAUNCH|EXIT):(?P<ptype>\w+):(?P<ppid>\d+)(?::(?P<rest>[^\r\n]*))?"
    r"|PROCESS:TERMINATE:(?P<ttype>\w+):(?P<tpid>\d+):group=(?P<group>-?\d+):pid=(?P<single>-?\d+))", re.M)


def process_history(text: str, must_end: tuple[str, ...] = ("web",)) -> tuple[list[str], set[tuple[str, str]]]:
    """Follows every engine process through the log in order (issue #862): a launch makes it live, an
    exit ends it. LazyOS ids are task slots and are reused at once, so an id means a process only between
    its launch and its end: an exit of an id that is not live, or of another type than the launch, a
    launch of an id still live, a `terminate` that sent nothing to a live process (neither the group's
    kill nor the process's), and a process of a type in `must_end` still live at the end are all
    problems. The page's process (`web`) ends with its view; the network process belongs to webd, which
    outlives the window, so it is not asked to end. Returns the problems and the (type, id) pairs seen
    to end."""
    problems: list[str] = []
    live: dict[str, str] = {}
    ended: set[tuple[str, str]] = set()
    for match in PROCESS_EVENT.finditer(engine_text(text)):
        if match["kind"] == "LAUNCH":
            kind, pid = match["ptype"], match["ppid"]
            if pid in live:
                problems.append(f"the {kind} process {pid} was launched while the {live[pid]} process {pid} was still live")
            live[pid] = kind
            ended.discard((kind, pid))
        elif match["kind"] == "EXIT":
            kind, pid = match["ptype"], match["ppid"]
            if pid not in live:
                problems.append(f"an exit of the {kind} process {pid}, which was not live")
            elif live[pid] != kind:
                problems.append(f"an exit of the {kind} process {pid}, which was launched as {live.pop(pid)}")
            else:
                del live[pid]
                ended.add((kind, pid))
        elif match["ttype"]:
            kind, pid = match["ttype"], match["tpid"]
            if pid in live and match["group"] != "0" and match["single"] != "0":
                problems.append(f"WebKit's terminate of the {kind} process {pid} sent no signal "
                                f"(group errno {match['group']}, pid errno {match['single']})")
    for pid, kind in live.items():
        if kind in must_end:
            problems.append(f"the {kind} process {pid} was launched and never seen to end")
    return problems, ended


def view_ids(text: str, pattern: re.Pattern[str]) -> list[int]:
    return [int(m["view"]) for m in pattern.finditer(text)]


def judge_quit(text: str) -> list[str]:
    """Closing the window ends its view: `QUIT:PASS` (the client printed it after its window closed, its exit
    status was 0 and its `curl` child was gone: a `CHILD:<name>:ALIVE` line otherwise), every view webd made
    for the client was destroyed, and every web process the engine launched was seen to end
    (`process_history`; the network process is webd's and stays)."""
    problems = []
    if f"{P}:QUIT:PASS" not in text:
        problems.append(f"the window did not close cleanly: no {P}:QUIT:PASS (saw {re.findall(rf'{P}:QUIT:\\w+', text)})")
    for alive in re.findall(rf"{P}:CHILD:[^\r\n]*ALIVE[^\r\n]*", text):
        problems.append(f"a child outlived the window: {alive}")
    mine = [int(m["view"]) for m in VIEW_CREATE.finditer(text) if m["client"] == CLIENT_LABEL]
    destroyed = set(view_ids(text, VIEW_DESTROY))
    for view in mine:
        if view not in destroyed:
            problems.append(f"webd's view {view} of the window was never destroyed (no WEBD:VIEW:DESTROY view={view})")
    history, _ = process_history(text)
    return problems + history


def judge_first(text: str) -> list[str]:
    """Everything the first boot's serial log must show."""
    return (judge_start(text) + judge_pages(text) + judge_dialogs(text) + judge_launches(text)
            + judge_popups(text) + judge_downloads(text))


def judge_second(text: str) -> list[str]:
    """The second boot: the first boot's page loaded (from the HTTP cache, see `judge_servers`), the cookie came
    back, the history page loaded, the clear was accepted."""
    problems = judge_start(text)
    if COOKIE_KEPT not in titles(text):
        problems.append(f"the first boot's cookie did not come back: no {P}:TITLE:{COOKIE_KEPT} (seen: {titles(text)})")
    if OLDNET_TITLE not in titles(text):
        problems.append(f"the page of the first boot did not load from the cache: no {P}:TITLE:{OLDNET_TITLE} "
                        f"(seen: {titles(text)})")
    if not loaded(text, "about:history"):
        problems.append(f"no {P}:LOAD:about:history")
    if "History cleared" in text:
        pass
    for line in markers(text, "LAUNCH"):
        problems.append(f"something started another program: {P}:LAUNCH:{line}")
    return problems


def judge_servers(first, second, assets: list[str]) -> list[str]:
    """What the stand-in sites saw: the first boot's pages and pictures, the form's
    query, no cookie on the first boot's cookie request and the cookie on the second's."""
    problems = []
    browser = list(first.requests)

    def asked(requests, scheme: str, hosts, path: str) -> list:
        return [r for r in requests if r.scheme == scheme and r.host in hosts and r.path.split("?")[0] == path
                and r.method == "GET" and r.status == 200]

    if not asked(browser, "http", ("example.com",), "/"):
        problems.append("example.com never served / over HTTP")
    if not asked(browser, "https", ("theoldnet.com",), "/"):
        problems.append("theoldnet.com never served / over HTTPS")
    for path in assets:
        if not asked(browser, "https", OLDNET_HOSTS, path):
            problems.append(f"WebKit never fetched {path} from theoldnet.com")
    for path in ("/webplus/script.html", "/webplus/links.html", "/webplus/blank-target.html",
                 "/webplus/opened.html", sites.DOWNLOAD_PATH, sites.SLOW_DOWNLOAD_PATH,
                 "/webkit/tall.html", "/webkit/links.html", "/webkit/form.html"):
        if not asked(browser, "https", ("theoldnet.com",), path):
            problems.append(f"WebKit never fetched {path}")
    typed = [r for r in asked(browser, "https", ("theoldnet.com",), "/webkit/echo")
             if parse_qs(urlsplit(r.path).query).get("q") == [INPUT_TEXT]]
    if not typed:
        problems.append(f"the form's query /webkit/echo?q={INPUT_TEXT} never reached the server")
    for r in browser:
        if r.scheme == "https" and r.sni != r.host:
            problems.append(f"{r.path}: SNI {r.sni!r} on a connection for Host {r.host!r}")
    agents = {r.user_agent for r in browser if r.user_agent}
    if not any("AppleWebKit" in agent for agent in agents):
        problems.append(f"the requests do not look like WebKit's: {sorted(agents)}")
    first_cookie = asked(first.requests, "https", ("theoldnet.com",), sites.COOKIE_PATH)
    if not first_cookie:
        problems.append(f"the first boot never asked for {sites.COOKIE_PATH}")
    elif any(sites.COOKIE_NAME in r.cookie for r in first_cookie):
        problems.append("the cookie page saw a cookie on the first boot, before it had set one")
    # A cacheable page the first boot fetched comes from the HTTP cache, which is what makes the
    # second boot a test of the cache-hit path (a hit refreshes the record's modification time):
    # never serve it `no-store` to make a run pass.
    for path in ("/", *assets):
        if asked(second.requests, "https", OLDNET_HOSTS, path):
            problems.append(f"the second boot asked theoldnet.com for {path} again: the HTTP cache did not serve "
                            "the first boot's copy, so the cache-hit path was not exercised")
    second_cookie = asked(second.requests, "https", ("theoldnet.com",), sites.COOKIE_PATH)
    if not second_cookie:
        problems.append(f"the second boot never asked for {sites.COOKIE_PATH}")
    elif not any(f"{sites.COOKIE_NAME}={sites.COOKIE_VALUE}" in r.cookie for r in second_cookie):
        problems.append("the first boot's cookie was not sent on the second boot: the profile did not keep it "
                        f"(Cookie headers seen: {[r.cookie for r in second_cookie]})")
    return problems


def history_urls(history: str) -> list[str]:
    """The URLs of a `history.tsv` (`time<TAB>url<TAB>title` lines)."""
    return [fields[1] for fields in (line.split("\t") for line in history.splitlines()) if len(fields) >= 2]


def judge_profile(listing: dict[str, bytes | None], first_history: str, second_history: str) -> list[str]:
    """The OS volume's evidence (`listing` maps a name under the home to its bytes, or None when absent):
    the first boot's history holds its pages, both downloads are whole with no `.part`, the cookie
    database exists (in webd's browser profile, `COOKIE_KEY`), and the second boot's Clear history left the file empty."""
    problems = []
    urls = history_urls(first_history)
    for url in (EXAMPLE_URL, OLDNET_URL, COOKIE_URL, LINKS_URL, SCRIPT_URL):
        if not any(seen.rstrip("/") == url.rstrip("/") for seen in urls):
            problems.append(f"{url} is not in the history file after the first boot (urls: {urls})")
    if any(url.startswith(("about:", "data:")) for url in urls):
        problems.append(f"the browser's own pages are in the history: {urls}")
    if urls.count(EXAMPLE_URL) > 1:
        problems.append("a page is in the history twice in a row")
    if DOWNLOAD_NAME in " ".join(urls) or sites.DOWNLOAD_PATH in " ".join(urls):
        problems.append("a download is in the history as a page")
    for name, size in ((DOWNLOAD_NAME, DOWNLOAD_BYTES), (SLOW_NAME, SLOW_BYTES)):
        data = listing.get(f"Downloads/{name}")
        if data is None:
            problems.append(f"~/Downloads/{name} is missing")
        elif len(data) != size:
            problems.append(f"~/Downloads/{name} is {len(data)} bytes, expected {size}")
        if listing.get(f"Downloads/{name}.part") is not None:
            problems.append(f"~/Downloads/{name}.part was left behind")
    if listing.get(COOKIE_KEY) is None:
        problems.append(f"webd's browser profile has no {COOKIE_DB} (~/{WEBD_PROFILE}): cookies were not persisted")
    if history_urls(second_history):
        problems.append(f"Clear history left {len(history_urls(second_history))} entries in {HISTORY_FILE}")
    return problems


def window_region(text: str, last: bool = False) -> tuple[int, int, int, int] | None:
    """The page area on the screen, from the probe lines."""
    pages = []
    for match in session_pointer._WIDGET.finditer(text):  # noqa: SLF001 - one regex for both readers
        x, y, w, h, name, window = match.groups()
        if name == "page" and window.strip().endswith(APP):
            pages.append((match.start(), window.strip(), int(x), int(y), int(w), int(h)))
    if not pages:
        return None
    at, title, x, y, w, h = pages[-1] if last else pages[0]
    rects = [m for m in session_pointer._RECT.finditer(text[:at]) if m.group(5).strip() == f"window:{title}"]  # noqa: SLF001
    if not rects:
        return None
    left, top = (int(n) for n in rects[-1].groups()[:2])
    return left + x, top + y, w, h


def judge_shots(paths: list[Path], region: tuple[int, int, int, int] | None, min_ink: float = 0.002) -> list[str]:
    """The page area of each page screenshot has ink and no two different pages look the same."""
    if len(paths) < 6:
        return [f"{len(paths)} screenshot(s), expected one per page"]
    if region is None:
        return ["the window's page area is unknown: no UI:WIDGET page / UI:RECT window line for it"]
    problems, digests = [], {}
    for path in paths:
        name = Path(path).name
        try:
            stats = region_stats(Path(path), region)
        except (OSError, ValueError) as error:
            problems.append(f"{name}: {error}")
            continue
        digests[name] = stats["digest"]
        if stats["nonbackground_ratio"] < min_ink:
            problems.append(f"{name}: the page area is blank ({stats['nonbackground_ratio']} ink)")
        if name.startswith("02_theoldnet") and stats["distinct_colors_q4"] < 16:
            problems.append(f"{name}: {stats['distinct_colors_q4']} colours, expected 16+ for the retro page")
    if len(set(digests.values())) < len(digests) - 4:
        problems.append("most of the page screenshots are identical: the pages never changed")
    return problems


def first_paint_ms(text: str) -> list[int]:
    return [int(n) for n in re.findall(r"^INFO:TIME:firstpaint=\d+:sincecommit=\d+:sincenav=(\d+)", engine_text(text), re.M)]


def load_ms(text: str) -> list[int]:
    return [int(n) for n in re.findall(r"^INFO:TIME:nav=\d+:start=\d+:commit=\d+:finish=\d+:load=(\d+)", engine_text(text), re.M)]
