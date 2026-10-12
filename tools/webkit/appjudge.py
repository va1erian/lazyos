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
        problems.append(f"WebKit reported a failure: {failure}")
    for failure in re.findall(r"WEBKIT:(?:W5:UI:)?(?:BIND|RUN):FAIL[^\r\n]*", text):
        problems.append(f"the window failed: {failure}")
    titles = lines(text, "WEBKIT:W5:TITLE:")
    for url, title in ((EXAMPLE_URL, EXAMPLE_TITLE), (OLDNET_URL, oldnet_title), (JS_URL, JS_TITLE),
                       (LINKS_URL, LINKS_TITLE), (FORM_URL, FORM_TITLE)):
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
    for path in ("/webkit/js.html", "/webkit/links.html", "/webkit/form.html"):
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
    if agents and not any("AppleWebKit" in agent for agent in agents):
        problems.append(f"the requests do not look like WebKit's: {sorted(agents)}")
    return problems


def window_region(text: str) -> tuple[int, int, int, int] | None:
    """The page area on the screen, from the probe lines: the newest `UI:WIDGET name=page` of a
    window whose caption ends in the app's name, placed by the newest `UI:RECT` of that window."""
    best = None
    for match in session_pointer._WIDGET.finditer(text):  # noqa: SLF001 - one regex for both readers
        x, y, w, h, name, window = match.groups()
        if name == "page" and window.strip().endswith(APP):
            best = (window.strip(), int(x), int(y), int(w), int(h))
    if best is None:
        return None
    title, x, y, w, h = best
    rect = session_pointer.probe_rect(text, f"window:{title}")
    if rect is None:
        return None
    return rect[0] + x, rect[1] + y, w, h


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


def judge_scroll(shots: dict[str, Path], region: tuple[int, int, int, int] | None) -> list[str]:
    """The wheel moved the page: the scrolled shot's page area differs from the one before it."""
    before, after = shots.get("before"), shots.get("after")
    if before is None or after is None or region is None:
        return ["the scroll shots or the page area are missing"]
    try:
        a, b = region_stats(before, region), region_stats(after, region)
    except (OSError, ValueError) as error:
        return [str(error)]
    return [] if a["digest"] != b["digest"] else ["the page looks the same after the wheel turned"]
