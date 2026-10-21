"""What the harnesses' stand-in sites serve, read from `fixtures/`.

`tools/webplus` (LazyWeb+), among others, browses two stand-ins for real
sites, served from the host by `sites.py`: example.com (plain HTTP) and a
90s-style theoldnet.com (HTTPS, with pictures and a style sheet). This module
says what their home pages are called and which same-site files they pull in,
so a judge can check that the browser asked for every one of them.
"""

from __future__ import annotations

import re
from html.parser import HTMLParser
from pathlib import Path

HERE = Path(__file__).resolve().parent
FIXTURES = HERE / "fixtures"
EXAMPLE_URL = "http://example.com/"
OLDNET_URL = "https://theoldnet.com/"
OLDNET_HOSTS = ("theoldnet.com", "www.theoldnet.com")


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
