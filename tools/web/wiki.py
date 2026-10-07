"""The Wikipedia copies of the LazyWeb harness (issue #632): serving them from
the host and judging what the browser did with them.

`wikicapture.py` saved two English Wikipedia pages, the Main Page and "1762",
as LazyWeb asks for them (`?useskin=vector`, the 2010 Vector skin that
NetSurf lays out well; `xui-app/web/src/sites.rs`), with their style sheets
and pictures, under `fixtures/wikipedia/`. The HTTPS stand-in (`sites.py`)
answers for en.wikipedia.org, upload.wikimedia.org and thumb.wikimedia.org
from that copy; plain HTTP redirects to HTTPS, like the real sites.

The browser keeps the URL it was given (`/wiki/1762`), so `WEB:LOAD` names
that, while the server must see `useskin=vector` on every page request: the
rewrite happens below NetSurf, at the fetch.
"""

from __future__ import annotations

import functools
import hashlib
import html
import json
import re
import sys
from pathlib import Path
from urllib.parse import parse_qsl, unquote, urlencode, urljoin, urlsplit

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "screenshot"))
import pngstats  # noqa: E402
from wikicapture import OUT as FIXTURE, PAGES, Refs, wanted  # noqa: E402

HOSTS = ("en.wikipedia.org", "upload.wikimedia.org", "thumb.wikimedia.org")
MAIN_URL = "https://en.wikipedia.org/wiki/Main_Page"
ARTICLE_URL = "https://en.wikipedia.org/wiki/1762"
#: The query LazyWeb adds to every wiki page it fetches.
SKIN = ("useskin", "vector")
#: The query the harness's own `curl` requests carry (`judge.PRECHECK_TAG`).
PRECHECK_TAG = "precheck"


@functools.lru_cache(maxsize=1)
def manifest() -> dict[str, dict]:
    """{normalised URL: {"file", "type"}} of everything captured."""
    raw = json.loads((FIXTURE / "manifest.json").read_text(encoding="utf-8"))
    return {_normalise(url): entry for url, entry in raw.items()}


def _normalise(url: str, drop: tuple[str, ...] = (PRECHECK_TAG,)) -> str:
    """`url` with percent-encoding undone and the query parameters `drop`
    names (the harness's own) left out, so a request matches however
    NetSurf escaped it."""
    parts = urlsplit(url)
    query = [(k, v) for k, v in parse_qsl(parts.query, keep_blank_values=True)
             if k not in drop]
    tail = f"?{unquote(urlencode(query))}" if query else ""
    return f"https://{parts.hostname}{unquote(parts.path)}{tail}"


def lookup(host: str, path: str) -> tuple[str, bytes] | None:
    """(content type, body) of the copy of `https://<host><path>`, or None.
    A `/wiki/` URL that is no page (the tracking pixel the Main Page shows
    without JavaScript) also gets the skin parameter from LazyWeb, which
    the copy, saved under the URL the page names, does not have."""
    url = f"https://{host}{path}"
    entry = manifest().get(_normalise(url)) or _bare().get(_normalise(url, _NOT_COPIED))
    if entry is None:
        return None
    return entry["type"], (FIXTURE / "files" / entry["file"]).read_bytes()


#: Query parameters a request may carry that the copy's URLs do not.
_NOT_COPIED = (PRECHECK_TAG, SKIN[0])


@functools.lru_cache(maxsize=1)
def _bare() -> dict[str, dict]:
    """The manifest keyed by URL without the skin parameter."""
    return {_normalise(url, _NOT_COPIED): entry for url, entry in manifest().items()}


def picture_bytes(url: str) -> bytes:
    """The copy of a picture, by its absolute URL."""
    parts = urlsplit(url)
    found = lookup(parts.hostname, url[url.index(parts.netloc) + len(parts.netloc):])
    if found is None:
        raise KeyError(f"no copy of {url}")
    return found[1]


def _page(url: str) -> str:
    """The copy of `url` (without the skin parameter) as text."""
    found = lookup("en.wikipedia.org", f"{urlsplit(url).path}?{urlencode([SKIN])}")
    if found is None:
        raise KeyError(f"no copy of {url} in {FIXTURE}")
    return found[1].decode("utf-8", "replace")


def title(url: str) -> str:
    """The `<title>` of the copy of a page."""
    match = re.search(r"<title>(.*?)</title>", _page(url), re.S)
    return " ".join(html.unescape(match.group(1)).split()) if match else ""


def resources(url: str) -> tuple[list[str], list[str]]:
    """(style sheets, pictures) the copy of `url` names in its HTML, as
    absolute URLs, on the hosts the capture copies (`wikicapture.wanted`).
    The rule is the capture's, not what it saved, so a resource the capture
    failed to save is still expected and fails the judge. The page's icon is
    not: Blitz draws no favicon, so it never asks for one."""
    refs = Refs()
    refs.feed(_page(url))

    def kept(found: list[str]) -> list[str]:
        return sorted({u for u in (urljoin(url, ref) for ref in found) if wanted(u)})

    pictures = [ref for ref in refs.pictures if ref not in refs.icons]
    return kept(refs.styles), kept(pictures)


@functools.lru_cache(maxsize=1)
def wiki_picture() -> str:
    """A picture of the article, for the harness's own check of the picture
    host. Looked up when a check needs it, so `--live` runs (and imports)
    never read the copies."""
    return next(url for url in resources(ARTICLE_URL)[1]
                if urlsplit(url).hostname == "thumb.wikimedia.org")


#: Every Liberation face LazyWeb draws pages with (`xui-app/web/src/fonts.rs`).
FONTS = "WEB:FONTS:12/12"


def judge_serial(text: str) -> list[str]:
    """Both pages loaded under the URL typed, with the copy's title, and
    drawn with the Liberation fonts."""
    lines = [line.strip() for line in text.splitlines()]
    problems = [] if FONTS in lines else [f"no {FONTS}: the web fonts did not all load"]
    for url in (MAIN_URL, ARTICLE_URL):
        if f"WEB:LOAD:{url}" not in lines:
            problems.append(f"no WEB:LOAD:{url}")
        if f"WEB:TITLE:{title(url)}" not in lines:
            problems.append(f"no WEB:TITLE:{title(url)}")
    return problems


def judge_servers(record) -> list[str]:
    """What the host saw: each page asked for in the 2010 skin, never in
    the default one, and every style sheet and picture of both pages."""
    browser = [r for r in record.requests if r.host in HOSTS and PRECHECK_TAG not in r.path]
    got = {_normalise(f"https://{r.host}{r.path}") for r in browser
           if r.scheme == "https" and r.method == "GET" and r.status == 200}
    problems = []
    for page in PAGES:
        if _normalise(page) not in got:
            problems.append(f"the browser never fetched {page}")
        styles, pictures = resources(page.split("?")[0])
        bare = {_normalise(u, _NOT_COPIED) for u in got}
        for url in styles + pictures:
            if _normalise(url) not in got and _normalise(url, _NOT_COPIED) not in bare:
                problems.append(f"the browser never fetched {url}")
    for r in browser:
        query = dict(parse_qsl(urlsplit(r.path).query))
        if r.path.startswith("/wiki/") and query.get(SKIN[0]) != SKIN[1]:
            problems.append(f"{r.host}{r.path} was asked for without useskin=vector")
        if r.scheme == "https" and r.sni != r.host:
            problems.append(f"{r.path}: SNI {r.sni!r} on a connection for Host {r.host!r}")
    return problems


def judge_shots(paths: list[Path], min_content: float = 0.02, min_colors: int = 24) -> list[str]:
    """The two pages' screenshots: each drawn, with the colours of its
    pictures and logo, and different from each other."""
    if len(paths) != 2:
        return [f"{len(paths)} Wikipedia screenshot(s), expected 2"]
    problems, digests = [], []
    for path in paths:
        try:
            width, height, channels, pixels = pngstats.decode_png(Path(path))
        except (OSError, ValueError) as error:
            problems.append(f"{path}: {error}")
            continue
        stats = pngstats.analyse(width, height, channels, pixels)
        digests.append(hashlib.sha256(pixels).hexdigest())
        if stats["nonbackground_ratio"] < min_content:
            problems.append(f"{Path(path).name}: almost black ({stats['nonbackground_ratio']})")
        if stats["distinct_colors_q4"] < min_colors:
            problems.append(f"{Path(path).name}: {stats['distinct_colors_q4']} colours, "
                            f"expected {min_colors}+")
    if len(digests) == 2 and digests[0] == digests[1]:
        problems.append("both Wikipedia screenshots are identical")
    return problems
