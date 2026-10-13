"""The WebKit browser harness's verdict (`app_run.py`, W5), as pure functions with their own
tests (`test_app_judge.py`).

Three kinds of evidence, each judged on its own, as for LazyWeb (`tools/web/judge.py`):

* **serial** - the UI process's markers: `WEBKIT:W5:UP`, the window's `WEBKIT:W5:UI:UP`, both
  child processes (`WEBPROCESS:UP:<pid>`, `NETWORKPROCESS:UP:<pid>`), then for each page
  `LOAD:<url>` and `TITLE:<title>`; `JS:<document.title>` read back by running script in the
  page; `CLICK:<url>` for a link followed with the mouse and `INPUT:<title>` for a form
  submitted from the keyboard (the echo page's title carries what was typed); never
  `WEBKIT:W5:FAIL:...`;
* **servers** - what the host's stand-in sites recorded (`tools/web/sites.py`): the WebProcess
  and NetworkProcess really fetched example.com over HTTP, theoldnet.com over HTTPS with SNI
  matching the Host header and every picture and style sheet its page references, the script,
  link and form pages, and the form's query (`/webkit/echo?q=...`, which only a keyboard
  that reached a text field could have typed);
* **screenshots** - the page area of each shot is not blank (`region`), the retro page is
  colourful, and the pages differ from one another.
"""

from __future__ import annotations

import hashlib
import re
import sys
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / "tools" / "web"))
sys.path.insert(0, str(ROOT / "tools" / "screenshot"))
import judge as web_judge  # noqa: E402
import pngstats  # noqa: E402
import session_pointer  # noqa: E402

EXAMPLE_URL = web_judge.EXAMPLE_URL
EXAMPLE_TITLE = web_judge.EXAMPLE_TITLE
OLDNET_URL = web_judge.OLDNET_URL
OLDNET_TITLE = web_judge.OLDNET_TITLE
JS_URL = "https://theoldnet.com/webkit/js.html"
#: The script's own result: 1^2 + ... + 10^2.
JS_TITLE = "js-on:385"
LINKS_URL = "https://theoldnet.com/webkit/links.html"
LINKS_TITLE = "WebKit links"
FORM_URL = "https://theoldnet.com/webkit/form.html"
FORM_TITLE = "WebKit form"
#: A tall page of static bands: the wheel moves them by a measurable number of rows.
TALL_URL = "https://theoldnet.com/webkit/tall.html"
TALL_TITLE = "WebKit tall"
#: A page that never answers in time (`sites.SLOW_PATH`): Stop ends it, and that is no failure.
SLOW_URL = "https://theoldnet.com/webkit/slow"
#: The host accepts a connection here and hangs up (`sites.Closer`): the load fails at once and the
#: window says so.
CLOSER_PORT = 81
FAIL_URL = f"http://example.com:{CLOSER_PORT}/"
#: What the session types into the form; the echo page's title is `echo:` + it.
INPUT_TEXT = "hello lazyos"
ECHO_TITLE = f"echo:{INPUT_TEXT}"
#: The window's caption for a page title.
APP = "WebKit"


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


def loaded(text: str, url: str) -> bool:
    return any(seen.rstrip("/") == url.rstrip("/") for seen in lines(text, "WEBKIT:W5:LOAD:"))


def judge_serial(text: str, oldnet_title: str = OLDNET_TITLE) -> list[str]:
    """The UI process's markers for the whole session."""
    problems = []
    for marker in ("WEBKIT:W5:UP", "WEBKIT:W5:UI:UP"):
        if marker not in text:
            problems.append(f"no {marker}")
    for kind in ("WEBPROCESS", "NETWORKPROCESS"):
        pids = re.findall(rf"WEBKIT:W5:{kind}:UP:(\d+)", text)
        if not pids:
            problems.append(f"no WEBKIT:W5:{kind}:UP:<pid>")
    for failure in lines(text, "WEBKIT:W5:FAIL:"):
        if not expected_failure(failure):
            problems.append(f"WebKit reported a failure: {failure}")
    for failure in re.findall(r"WEBKIT:(?:W5:UI:)?(?:BIND|RUN):FAIL[^\r\n]*", text):
        problems.append(f"the window failed: {failure}")
    titles = lines(text, "WEBKIT:W5:TITLE:")
    for url, title in ((EXAMPLE_URL, EXAMPLE_TITLE), (OLDNET_URL, oldnet_title), (TALL_URL, TALL_TITLE),
                       (JS_URL, JS_TITLE), (LINKS_URL, LINKS_TITLE), (FORM_URL, FORM_TITLE)):
        if not loaded(text, url):
            problems.append(f"no WEBKIT:W5:LOAD:{url}")
        if title not in titles:
            problems.append(f"no WEBKIT:W5:TITLE:{title} (titles seen: {titles})")
    scripted = lines(text, "WEBKIT:W5:JS:")
    if JS_TITLE not in scripted:
        problems.append(f"script did not run in the page: no WEBKIT:W5:JS:{JS_TITLE} (seen: {scripted})")
    if "js-off" in scripted:
        problems.append("a page read back its static title: its script did not run before the readback")
    clicks = lines(text, "WEBKIT:W5:CLICK:")
    if FORM_URL not in clicks:
        problems.append(f"no WEBKIT:W5:CLICK:{FORM_URL} (clicks seen: {clicks})")
    echoed = lines(text, "WEBKIT:W5:INPUT:")
    if ECHO_TITLE not in echoed:
        problems.append(f"the typed text did not come back: no WEBKIT:W5:INPUT:{ECHO_TITLE} (seen: {echoed})")
    return problems


def expected_failure(failure: str) -> bool:
    """The one load the session makes fail on purpose (`FAIL_URL`)."""
    return failure.startswith(f"provisional:{FAIL_URL}:")


def judge_errors(text: str) -> list[str]:
    """A load that fails is reported (`FAIL`, the window's `UI:ERROR`) and a load the user stops
    is not: one failure for `FAIL_URL`, one error line in the window, a `CANCELLED` for the slow
    page, which never finished loading."""
    problems = []
    failed = [f for f in lines(text, "WEBKIT:W5:FAIL:") if expected_failure(f)]
    if not failed:
        problems.append(f"no WEBKIT:W5:FAIL:provisional:{FAIL_URL}: for the address that hangs up")
    errors = lines(text, "WEBKIT:W5:UI:ERROR:")
    if not any(e.startswith(f"Could not load {FAIL_URL}") for e in errors):
        problems.append(f"the window never showed the error (UI:ERROR lines: {errors})")
    if len(errors) > 1:
        problems.append(f"the window showed more than one error: {errors} (a stopped load is not one)")
    if f"WEBKIT:W5:START:{SLOW_URL}" not in text:
        problems.append(f"the slow page's load never started: no WEBKIT:W5:START:{SLOW_URL}")
    if not lines(text, "WEBKIT:W5:CANCELLED:"):
        problems.append("the stopped load was not reported as cancelled: no WEBKIT:W5:CANCELLED")
    if loaded(text, SLOW_URL):
        problems.append("the stopped page loaded anyway")
    return problems


def judge_quit(text: str) -> list[str]:
    """Closing the window ends the run loop and both child processes: `QUIT:PASS`, and for each
    pid the UI process named, a `CHILD:<name>:<pid>:GONE` (what its own waitpid/kill saw)."""
    problems = []
    if "WEBKIT:W5:QUIT:PASS" not in text:
        quits = re.findall(r"WEBKIT:W5:QUIT:\w+", text)
        problems.append(f"the window did not close cleanly: no WEBKIT:W5:QUIT:PASS (saw {quits})")
    for name, kind in (("web", "WEBPROCESS"), ("network", "NETWORKPROCESS")):
        for pid in sorted(set(re.findall(rf"WEBKIT:W5:{kind}:UP:(\d+)", text))):
            if f"WEBKIT:W5:CHILD:{name}:{pid}:GONE" not in text:
                problems.append(f"the {name} process {pid} was not seen to end: no WEBKIT:W5:CHILD:{name}:{pid}:GONE")
    for alive in re.findall(r"WEBKIT:W5:CHILD:\w+:\d+:ALIVE", text):
        problems.append(f"a child outlived the window: {alive}")
    return problems


def resizes(text: str) -> list[tuple[int, int]]:
    """The sizes the engine laid the page out at (`INFO:RESIZE`), in order."""
    return [(int(w), int(h)) for w, h in re.findall(r"WEBKIT:W5:INFO:RESIZE:(\d+)x(\d+)", text)]


def judge_resize(text: str) -> list[str]:
    """Maximizing the window made the engine lay the page out larger: the last size is wider and
    taller than the first."""
    sizes = resizes(text)
    if len(sizes) < 2:
        return [f"the engine was told of {len(sizes)} size(s), expected the window's and the maximized one"]
    (w0, h0), (w1, h1) = sizes[0], sizes[-1]
    if w1 <= w0 or h1 <= h0:
        return [f"the page was not laid out larger after the window grew: {sizes}"]
    return []


def first_paint_ms(text: str) -> list[int]:
    """The `sincenav` milliseconds of every `INFO:TIME:firstpaint` line, in order."""
    return [int(n) for n in re.findall(r"WEBKIT:W5:INFO:TIME:firstpaint=\d+:sincecommit=\d+:sincenav=(\d+)", text)]


def load_ms(text: str) -> list[int]:
    """The `load` milliseconds (navigation start to finish) of every finished navigation."""
    return [int(n) for n in re.findall(r"WEBKIT:W5:INFO:TIME:nav=\d+:start=\d+:commit=\d+:finish=\d+:load=(\d+)", text)]


def judge_servers(record, assets: list[str]) -> list[str]:
    """What the stand-in sites saw from WebKit's NetworkProcess (`sites.Record`)."""
    problems = []
    browser = list(record.requests)

    def asked(scheme: str, hosts, path: str) -> list:
        return [r for r in browser if r.scheme == scheme and r.host in hosts
                and r.path.split("?")[0] == path and r.method == "GET" and r.status == 200]

    if not asked("http", ("example.com",), "/"):
        problems.append("example.com never served / over HTTP with Host: example.com")
    if not asked("https", ("theoldnet.com",), "/"):
        problems.append("theoldnet.com never served / over HTTPS with Host: theoldnet.com")
    for path in assets:
        if not asked("https", web_judge.OLDNET_HOSTS, path):
            problems.append(f"WebKit never fetched {path} from theoldnet.com")
    for path in ("/webkit/tall.html", "/webkit/js.html", "/webkit/links.html", "/webkit/form.html"):
        if not asked("https", ("theoldnet.com",), path):
            problems.append(f"WebKit never fetched {path}")
    typed = [r for r in asked("https", ("theoldnet.com",), "/webkit/echo")
             if parse_qs(urlsplit(r.path).query).get("q") == [INPUT_TEXT]]
    if not typed:
        problems.append(f"the form's query /webkit/echo?q={INPUT_TEXT} never reached the server")
    for r in browser:
        if r.scheme == "https" and r.sni != r.host:
            problems.append(f"{r.path}: SNI {r.sni!r} on a connection for Host {r.host!r}")
    if not any(sni == "theoldnet.com" for sni in record.handshakes):
        problems.append("no TLS handshake with SNI theoldnet.com")
    agents = {r.user_agent for r in browser if r.user_agent}
    if not agents:
        problems.append("no request carried a User-Agent: cannot tell WebKit's requests from anyone's")
    elif not any("AppleWebKit" in agent for agent in agents):
        problems.append(f"the requests do not look like WebKit's: {sorted(agents)}")
    return problems


def window_region(text: str, last: bool = False) -> tuple[int, int, int, int] | None:
    """The page area on the screen, from the probe lines: the first (`last`: the newest)
    `UI:WIDGET name=page` of a window whose caption ends in the app's name, placed by the
    `UI:RECT` of that window printed nearest to it (the window is announced, then its controls)."""
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


def region_stats(path: Path, region: tuple[int, int, int, int]) -> dict:
    """`pngstats` numbers for a rectangle of a screenshot, against a white background."""
    width, height, channels, pixels = pngstats.decode_png(Path(path))
    x, y, w, h = region
    x0, y0, x1, y1 = max(x, 0), max(y, 0), min(x + w, width), min(y + h, height)
    if x1 <= x0 or y1 <= y0:
        raise ValueError(f"region {region} is outside the {width}x{height} screenshot")
    rows = [pixels[(row * width + x0) * channels:(row * width + x1) * channels] for row in range(y0, y1)]
    crop = b"".join(rows)
    stats = pngstats.analyse(x1 - x0, y1 - y0, channels, crop, bg=(255, 255, 255))
    stats["digest"] = hashlib.sha256(crop).hexdigest()
    return stats


def judge_shots(paths: list[Path], region: tuple[int, int, int, int] | None, min_ink: float = 0.002,
                min_colors_oldnet: int = 16) -> list[str]:
    """The page area of each screenshot (the shots of `app_session.py`, in page order: example,
    theoldnet, theoldnet later, scrolled, script, form, typed, echo) has ink, the retro page is
    colourful, and no two different pages look the same."""
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
        if "theoldnet" in name and "later" not in name and stats["distinct_colors_q4"] < min_colors_oldnet:
            problems.append(f"{name}: {stats['distinct_colors_q4']} colours, expected {min_colors_oldnet}+ "
                            "for the retro page")
    named = {n: d for n, d in digests.items() if "later" not in n}
    if len(set(named.values())) != len(named):
        problems.append("two of the page screenshots are identical: a page never changed")
    return problems


def judge_maximized(shot: Path | None, region: tuple[int, int, int, int] | None, strip: int = 120,
                    min_ink: float = 0.5) -> list[str]:
    """The maximized window's page area is painted all the way to its right edge: the page's own
    background (not the window's white) fills the last `strip` pixels, which the page's old size
    did not reach."""
    if shot is None or region is None:
        return ["the maximized screenshot or its page area is missing"]
    x, y, w, h = region
    if w < 1100:
        return [f"the page area is {w} px wide after maximizing"]
    try:
        stats = region_stats(shot, (x + w - strip, y, strip, h))
    except (OSError, ValueError) as error:
        return [str(error)]
    if stats["nonbackground_ratio"] < min_ink:
        return [f"the right {strip} px of the maximized page are blank ({stats['nonbackground_ratio']} ink): "
                "the page was not painted at the new size"]
    return []


#: How far five wheel notches move the page, in pixels: WebKit's line step of 40 px per notch
#: (`Scrollbar::pixelsPerLineStep`), measured in a run (docs/webkit-port-plan.md, W5).
SCROLL_PX: int | None = 200
#: The left part of the page area that a scroll is measured on: the scroll bar's thumb moves with
#: the page and the pointer (drawn into the screenshot) rests further right.
SCROLL_COLUMNS = 300


def crop_rows(path: Path, region: tuple[int, int, int, int], columns: int = SCROLL_COLUMNS) -> list[bytes]:
    """The rows of the left `columns` pixels of a rectangle of a screenshot, each as bytes."""
    width, height, channels, pixels = pngstats.decode_png(Path(path))
    x, y, w, h = region
    x0, y0, x1, y1 = max(x, 0), max(y, 0), min(x + min(w, columns), width), min(y + h, height)
    if x1 <= x0 or y1 <= y0:
        raise ValueError(f"region {region} is outside the {width}x{height} screenshot")
    return [bytes(pixels[(row * width + x0) * channels:(row * width + x1) * channels]) for row in range(y0, y1)]


def scroll_offset(before: list[bytes], after: list[bytes], need: float = 0.97) -> int | None:
    """By how many rows the picture moved up between two crops of a page of static content: the
    shift with the most identical rows, when at least `need` of the compared rows match. A page of
    animations or one that did not move gives None (a shift of 0 is not a scroll)."""
    rows = min(len(before), len(after))
    if sum(1 for y in range(rows) if after[y] == before[y]) >= need * rows:
        return None  # the same picture: static bands look alike a row apart, so say so first
    best, best_same = None, 0
    for shift in range(1, rows - rows // 4):
        compared = rows - shift
        same = sum(1 for y in range(compared) if after[y] == before[y + shift])
        if same > best_same:
            best, best_same = shift, same
    if best is None or best_same < need * (rows - best):
        return None
    return best


def judge_scroll(shots: dict[str, Path], region: tuple[int, int, int, int] | None,
                 expected: int | None = None) -> tuple[list[str], int | None]:
    """The wheel moved a page of static bands by a whole number of rows (`expected`, when known):
    the problems, and the shift measured."""
    before, after = shots.get("before"), shots.get("after")
    if before is None or after is None or region is None:
        return ["the scroll shots or the page area are missing"], None
    try:
        shift = scroll_offset(crop_rows(before, region), crop_rows(after, region))
    except (OSError, ValueError) as error:
        return [str(error)], None
    if shift is None:
        return ["the page did not move as one picture after the wheel turned (no row shift fits)"], None
    if expected is not None and shift != expected:
        return [f"the wheel moved the page {shift} px, expected {expected}"], shift
    return [], shift
