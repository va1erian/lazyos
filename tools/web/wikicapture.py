#!/usr/bin/env python3
"""Capture the Wikipedia pages the LazyWeb harness serves (issue #632).

The sandbox and CI cannot reach Wikipedia, so `run.py` serves copies of two
English Wikipedia pages from the host, the way it serves example.com: the Main
Page and the article "1762", as LazyWeb asks for them (the default skin, Vector
2022: Blitz lays it out, so there is no skin rewrite), with the style sheets and
pictures they reference, so the browser lays the copy out exactly as it would
the live page. This script fetches them into `fixtures/wikipedia/`:

    files/<sha256 prefix>.<ext>   each response body, as served
    manifest.json                 {"<url>": {"file": ..., "type": ...}, ...}

URLs are the absolute ones the page names (`&amp;` decoded); `wiki.py`
matches a request to them with percent-encoding undone, since a browser may
escape characters (`|` in `load.php`'s module lists) the page left bare.

The copies are Wikipedia content under CC BY-SA 4.0, and the pictures under
the licences their file pages on Wikimedia Commons give;
`fixtures/wikipedia/README.md` says so. Re-run to refresh them (the Main
Page changes daily; nothing in the harness depends on what it says):

    python tools/web/wikicapture.py
"""

from __future__ import annotations

import hashlib
import json
import re
import sys
import urllib.request
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urljoin, urlsplit

HERE = Path(__file__).resolve().parent
OUT = HERE / "fixtures" / "wikipedia"
#: The pages, as LazyWeb fetches them.
PAGES = ["https://en.wikipedia.org/wiki/Main_Page",
         "https://en.wikipedia.org/wiki/1762"]
#: Hosts whose resources are copied; anything else the page names is left out.
HOSTS = ("en.wikipedia.org", "upload.wikimedia.org", "thumb.wikimedia.org")
#: LazyWeb's user agent (`xui-app/web/src/fetch/mod.rs`): Wikipedia serves
#: by user agent in places, so ask as the browser does.
USER_AGENT = "Mozilla/5.0 (LazyOS) Blitz LazyWeb/0.1.0 (harness fixture capture)"
EXTENSIONS = {"text/html": ".html", "text/css": ".css", "image/png": ".png",
              "image/jpeg": ".jpg", "image/gif": ".gif", "image/svg+xml": ".svg",
              "image/x-icon": ".ico", "image/vnd.microsoft.icon": ".ico"}
CSS_URL = re.compile(r"""url\(\s*(['"]?)([^'")]+)\1\s*\)""")


class Refs(HTMLParser):
    """Style sheets and pictures a page references."""

    def __init__(self) -> None:
        super().__init__()
        self.styles: list[str] = []
        self.pictures: list[str] = []
        # The `<link rel=icon>` hrefs, also in `pictures` (the capture saves
        # them): a browser that draws no tab icon never fetches them.
        self.icons: list[str] = []

    def handle_starttag(self, tag, attrs) -> None:
        values = dict(attrs)
        rel = (values.get("rel") or "").lower().split()
        if tag == "link" and "stylesheet" in rel and values.get("href"):
            self.styles.append(values["href"])
        elif tag == "link" and "icon" in rel and values.get("href"):
            self.pictures.append(values["href"])
            self.icons.append(values["href"])
        elif tag == "img" and values.get("src"):
            self.pictures.append(values["src"])


class _CopiedHostsOnly(urllib.request.HTTPRedirectHandler):
    """Follows a redirect only to the copied hosts over HTTPS: the target is
    checked before any request goes to it."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if not wanted(newurl):
            raise OSError(f"redirected to {newurl}, outside the copied hosts")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


_OPENER = urllib.request.build_opener(_CopiedHostsOnly)


def fetch(url: str) -> tuple[bytes, str]:
    """The body and type of `url`. A redirect off the copied hosts (or
    HTTPS) is refused before it is followed, and the final URL is checked
    again, so nothing is saved under a URL it did not come from."""
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    with _OPENER.open(request, timeout=60) as response:
        final = response.geturl()
        if not wanted(final):
            raise OSError(f"redirected to {final}, outside the copied hosts")
        kind = response.headers.get("Content-Type", "application/octet-stream")
        return response.read(), kind


def wanted(url: str) -> bool:
    """Whether the harness copies `url`: HTTPS on the copied hosts, except the
    Main Page's tracking pixel, which redirects to another host (auth.wikimedia.org)."""
    parts = urlsplit(url)
    return (parts.scheme == "https" and parts.hostname in HOSTS
            and "/wiki/Special:CentralAutoLogin" not in parts.path)


def save(url: str, manifest: dict, store: Path) -> bytes | None:
    """Fetch `url` into the store once; its body, or None if it failed."""
    if url in manifest:
        return (store / manifest[url]["file"]).read_bytes()
    try:
        body, kind = fetch(url)
    except OSError as error:
        print(f"wikicapture: skipped {url}: {error}")
        return None
    ext = EXTENSIONS.get(kind.split(";")[0].strip().lower(), ".bin")
    name = hashlib.sha256(url.encode()).hexdigest()[:16] + ext
    (store / name).write_bytes(body)
    manifest[url] = {"file": name, "type": kind}
    print(f"wikicapture: {len(body):>7} {url}")
    return body


def capture() -> dict:
    store = OUT / "files"
    store.mkdir(parents=True, exist_ok=True)
    for stale in store.iterdir():
        stale.unlink()
    manifest: dict = {}
    for page in PAGES:
        html = save(page, manifest, store)
        if html is None:
            sys.exit(f"wikicapture: could not fetch {page}")
        refs = Refs()
        refs.feed(html.decode("utf-8", "replace"))
        for href in refs.styles:
            sheet_url = urljoin(page, href)
            sheet = save(sheet_url, manifest, store) if wanted(sheet_url) else None
            for _, ref in CSS_URL.findall((sheet or b"").decode("utf-8", "replace")):
                target = urljoin(sheet_url, ref.strip())
                if wanted(target):
                    save(target, manifest, store)
        for src in refs.pictures:
            target = urljoin(page, src)
            if wanted(target):
                save(target, manifest, store)
    (OUT / "manifest.json").write_text(json.dumps(manifest, indent=1, sort_keys=True) + "\n",
                                       encoding="utf-8")
    return manifest


if __name__ == "__main__":
    found = capture()
    total = sum((OUT / "files" / entry["file"]).stat().st_size for entry in found.values())
    print(f"wikicapture: {len(found)} files, {total} bytes in {OUT}")
