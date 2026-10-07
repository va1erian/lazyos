"""The LazyWeb harness's verdict (`run.py`), as pure functions with their own
tests (`test_judge.py`).

Three kinds of evidence, each judged on its own:

* **serial** - the browser's markers (`WEB:UP:PASS`, `WEB:LOAD:<url>`,
  `WEB:TITLE:<title>`, never `WEB:FAIL:<reason>`) and the harness's own
  `curl` checks typed into the Terminal first (`WEBH:<name>:PASS`), which
  prove the path to the host servers before the browser is blamed;
* **servers** - what the host's stand-in sites recorded (`sites.Record`): the
  browser asked example.com for `/` over plain HTTP, opened TLS to
  theoldnet.com with SNI matching the Host header, and fetched the page and
  every picture and style sheet it references (PNG, JPEG, GIF). Requests the
  harness's `curl` made carry `?precheck` and never count for the browser;
* **screenshots** - something was drawn (`pngstats`): each shot has content,
  the retro page is colourful, and the two pages do not look the same;
* **browser features** - LazyWeb was started by opening its URL through the
  OS (`sys::mimed::open` from `rhai`, `mimed` and `init`), downloaded the attachment
  whole, showed its history and downloads pages, and handed a `mailto:` link
  to the OS.
"""

from __future__ import annotations

import hashlib
import re
import sys
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urlsplit

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "screenshot"))
import pngstats  # noqa: E402
from sites import DOWNLOAD_NAME, DOWNLOAD_PATH, download_payload  # noqa: E402
import wiki  # noqa: E402

FIXTURES = HERE / "fixtures"
EXAMPLE_URL = "http://example.com/"
OLDNET_URL = "https://theoldnet.com/"
OLDNET_HOSTS = ("theoldnet.com", "www.theoldnet.com")
#: The query the harness's own `curl` requests carry.
PRECHECK_TAG = "precheck"


class _Refs(HTMLParser):
    """Same-site resources a page references: pictures, backgrounds, style sheets."""

    def __init__(self) -> None:
        super().__init__()
        self.refs: list[str] = []
        self.title = ""
        self._in_title = False

    def handle_starttag(self, tag, attrs) -> None:
        values = dict(attrs)
        self._in_title = tag == "title"
        for name in ("src", "background"):
            if values.get(name):
                self.refs.append(values[name])
        if tag == "link" and (values.get("rel") or "").lower() == "stylesheet":
            self.refs.append(values.get("href") or "")

    def handle_endtag(self, tag) -> None:
        self._in_title = False if tag == "title" else self._in_title

    def handle_data(self, data) -> None:
        if self._in_title:
            self.title += data


def _parse(site: str) -> _Refs:
    parser = _Refs()
    parser.feed((FIXTURES / site / "index.html").read_text(encoding="latin-1"))
    return parser


def fixture_title(site: str) -> str:
    """The `<title>` of a stand-in site's home page."""
    return " ".join(_parse(site).title.split())


def page_assets(site: str = "theoldnet.com") -> list[str]:
    """Every same-site path the home page and its style sheets reference."""
    refs = [ref for ref in _parse(site).refs if ref.startswith("/")]
    for sheet in [ref for ref in refs if ref.endswith(".css")]:
        css = (FIXTURES / site / sheet.lstrip("/")).read_text(encoding="utf-8")
        refs += [m.strip("'\"") for m in re.findall(r"url\(([^)]+)\)", css)]
    return sorted(set(refs))


OLDNET_TITLE = fixture_title("theoldnet.com")
EXAMPLE_TITLE = fixture_title("example.com")


def _lines(text: str, prefix: str) -> list[str]:
    return [line.strip()[len(prefix):] for line in text.splitlines()
            if line.strip().startswith(prefix)]


def _loaded(text: str, url: str) -> bool:
    return any(seen.rstrip("/") == url.rstrip("/") for seen in _lines(text, "WEB:LOAD:"))


def judge_prechecks(text: str, names: list[str]) -> list[str]:
    """The harness's `curl` checks: each printed `WEBH:<name>:PASS`."""
    problems = []
    for name in names:
        # Anywhere on a line: the console may interleave its own output.
        found = re.findall(rf"WEBH:{name}:(PASS|FAIL\S*)", text)
        if not found or found[-1] != "PASS":
            problems.append(f"check {name}: {found[-1] if found else 'never reported'}")
    return problems


def judge_serial(text: str, oldnet_title: str | None = OLDNET_TITLE) -> list[str]:
    """The browser's markers. `oldnet_title` None (`--live`) accepts any title
    other than example.com's for the second page."""
    problems = []
    if "WEB:UP:PASS" not in text:
        problems.append("the browser never reported WEB:UP:PASS")
    for failure in _lines(text, "WEB:FAIL:"):
        problems.append(f"the browser failed: {failure}")
    titles = _lines(text, "WEB:TITLE:")
    for url, title in ((EXAMPLE_URL, EXAMPLE_TITLE), (OLDNET_URL, oldnet_title)):
        if not _loaded(text, url):
            problems.append(f"no WEB:LOAD:{url}")
    if EXAMPLE_TITLE not in titles:
        problems.append(f"no WEB:TITLE:{EXAMPLE_TITLE} (titles seen: {titles})")
    if oldnet_title is not None and oldnet_title not in titles:
        problems.append(f"no WEB:TITLE:{oldnet_title} (titles seen: {titles})")
    if oldnet_title is None and not [t for t in titles if t and t != EXAMPLE_TITLE]:
        problems.append(f"no title for {OLDNET_URL} (titles seen: {titles})")
    return problems


def judge_servers(record, assets: list[str]) -> list[str]:
    """What the stand-in sites saw from the browser (`sites.Record`)."""
    problems = []
    browser = [r for r in record.requests if PRECHECK_TAG not in r.path]

    def asked(scheme: str, hosts, path: str) -> list:
        return [r for r in browser if r.scheme == scheme and r.host in hosts
                and r.path.split("?")[0] == path and r.method == "GET" and r.status == 200]

    if not asked("http", ("example.com",), "/"):
        problems.append("example.com never served / to the browser over HTTP with Host: example.com")
    if not asked("https", ("theoldnet.com",), "/"):
        problems.append("theoldnet.com never served / to the browser over HTTPS with Host: theoldnet.com")
    for path in assets:
        if not asked("https", OLDNET_HOSTS, path):
            problems.append(f"the browser never fetched {path} from theoldnet.com")
    for r in browser:
        if r.scheme == "https" and r.sni != r.host:
            problems.append(f"{r.path}: SNI {r.sni!r} on a connection for Host {r.host!r}")
        if not r.host:
            problems.append(f"{r.scheme} {r.path} came without a Host header")
    if not any(sni == "theoldnet.com" for sni in record.handshakes):
        problems.append("no TLS handshake with SNI theoldnet.com")
    return problems


def judge_shots(paths: list[Path], min_content: float = 0.02, min_colors: int = 8,
                min_colors_last: int = 24) -> list[str]:
    """The screenshots of the two pages, in order: each has content, the last
    (the retro page: tiles, a photo, a rainbow) is colourful, and the first
    and last differ."""
    if len(paths) < 2:
        return [f"{len(paths)} screenshot(s), expected one per page"]
    problems, digests = [], []
    for n, path in enumerate(paths):
        try:
            width, height, channels, pixels = pngstats.decode_png(Path(path))
        except (OSError, ValueError) as error:
            problems.append(f"{path}: {error}")
            continue
        stats = pngstats.analyse(width, height, channels, pixels)
        digests.append(hashlib.sha256(pixels).hexdigest())
        if stats["nonbackground_ratio"] < min_content:
            problems.append(f"{Path(path).name}: almost black ({stats['nonbackground_ratio']})")
        floor = min_colors_last if n == len(paths) - 1 else min_colors
        if stats["distinct_colors_q4"] < floor:
            problems.append(f"{Path(path).name}: {stats['distinct_colors_q4']} colours, expected {floor}+")
    if len(digests) == len(paths) and digests[0] == digests[-1]:
        problems.append("the first and last screenshots are identical: the page never changed")
    return problems


def precheck_requests() -> list[tuple[str, str, str, int]]:
    """(scheme, Host, path, status) of every request the check script makes.
    A function, so importing the judge never reads the Wikipedia copies."""
    return [
        ("http", "example.com", "/", 200),
        ("https", "theoldnet.com", "/", 200),
        ("http", "theoldnet.com", "/", 301),
        ("https", "www.theoldnet.com", "/style.css", 200),
        ("https", "theoldnet.com", "/images/logo.png", 200),
        ("https", "theoldnet.com", "/images/photo.jpg", 200),
        ("https", "theoldnet.com", "/images/construction.gif", 200),
        ("https", "en.wikipedia.org", urlsplit(wiki.ARTICLE_URL).path, 200),
        ("https", "thumb.wikimedia.org", urlsplit(wiki.wiki_picture()).path, 200),
    ]


def judge_precheck_servers(record) -> list[str]:
    """What the stand-in sites saw from the check script's `curl`: every
    request, under its own Host, with SNI naming that host on HTTPS."""
    problems = []
    tagged = [r for r in record.requests if PRECHECK_TAG in r.path]
    for scheme, host, path, status in precheck_requests():
        if not [r for r in tagged if (r.scheme, r.host, r.path.split("?")[0], r.status)
                == (scheme, host, path, status)]:
            problems.append(f"no {scheme}://{host}{path} answered {status} for the checks")
    for r in tagged:
        if r.scheme == "https" and r.sni != r.host:
            problems.append(f"{r.path}: SNI {r.sni!r} on a connection for Host {r.host!r}")
    return problems


#: The `mailto:` link the session types into the address bar.
MAILTO_URL = "mailto:webmaster@theoldnet.com"


def judge_features(text: str, record=None) -> list[str]:
    """The browser features beyond showing pages (see the module docstring).
    A `mailto:` link may find no app (an image without Mail); it must still
    have been handed to the OS."""
    problems = []
    if "OPEN:x-scheme-handler/http:os.lazy.lazyweb" not in text:
        problems.append("opening http://example.com/ through mimed did not pick LazyWeb")
    size = len(download_payload())
    # A reused image (`--no-build`) already holds the file: the browser then
    # saves it as "oldnet-kit (1).zip", and so on.
    stem, ext = DOWNLOAD_NAME.rsplit(".", 1)
    name = rf"{re.escape(stem)}(?: \(\d+\))?\.{re.escape(ext)}"
    if not re.search(rf"WEB:DOWNLOAD:START:{name}$", text, re.M):
        problems.append(f"the download of {DOWNLOAD_NAME} never started")
    if not re.search(rf"WEB:DOWNLOAD:DONE:{name}:{size}$", text, re.M):
        done = _lines(text, "WEB:DOWNLOAD:")
        problems.append(f"no WEB:DOWNLOAD:DONE:{DOWNLOAD_NAME}:{size} (saw {done})")
    for page in ("about:history", "about:downloads"):
        if not _loaded(text, page):
            problems.append(f"no WEB:LOAD:{page}")
    if not re.search(rf"WEB:LAUNCH:{re.escape(MAILTO_URL)}:(OK|FAIL)", text):
        problems.append(f"{MAILTO_URL} was never handed to the OS")
    if record is not None and not [r for r in record.requests if r.method == "GET"
                                   and r.path.split("?")[0] == DOWNLOAD_PATH
                                   and r.status == 200 and PRECHECK_TAG not in r.path]:
        problems.append(f"theoldnet.com never served {DOWNLOAD_PATH} to the browser")
    return problems
