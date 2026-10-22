"""Turn a link written in a Markdown source into a URL on the built site.

A link in the docs is written for GitHub's file browser: relative to the file,
to a `.md` source, often to code outside `docs/`. On the site it must reach the
built page, the copied file, or (for code) the file on GitHub.
"""

from __future__ import annotations

import posixpath
import re
from dataclasses import dataclass, field
from pathlib import Path
from urllib.parse import quote, unquote

SCHEME = re.compile(r"^(?:[a-zA-Z][a-zA-Z0-9+.\-]*:|//)")
HTML_URL = re.compile(r"""(\b(?:href|src)\s*=\s*)(["'])(.*?)\2""", re.I | re.S)


def relative_url(target: str, from_page: str) -> str:
    """URL of site path `target` as written on the page at `from_page`."""
    start = posixpath.dirname(from_page) or "."
    return posixpath.relpath(target, start)


@dataclass
class LinkResolver:
    root: Path
    repo_url: str
    ref: str
    pages: dict[str, str]  # repo-relative source -> site path
    generated: set[str] = field(default_factory=set)
    broken: list[tuple[str, str]] = field(default_factory=list)  # (source, href)
    missing_generated: set[str] = field(default_factory=set)
    anchors: list[tuple[str, str, str]] = field(default_factory=list)  # source, target page, fragment

    def github(self, rel: str, raw: bool = False) -> str:
        if raw:  # an image: the page's own bytes, not GitHub's HTML view of them
            kind = "raw"
        else:
            kind = "tree" if (self.root / rel).is_dir() else "blob"
        return f"{self.repo_url}/{kind}/{self.ref}/{quote(rel)}"

    def rewrite(self, href: str, src: str, out: str, raw: bool = False) -> str:
        """The URL to use for `href`, written in `src` and built to `out`; `raw`
        for something a page embeds (an image) rather than links to."""
        if href.startswith("#"):
            if len(href) > 1:
                self.anchors.append((src, out, unquote(href[1:])))
            return href
        if not href or SCHEME.match(href):
            return href
        raw_path, hash_, fragment = href.partition("#")
        path_part, query_sep, query = raw_path.partition("?")
        path = unquote(path_part)
        if path.startswith("/"):
            joined = path.lstrip("/")
        else:
            joined = posixpath.join(posixpath.dirname(src), path)
        rel = posixpath.normpath(joined)
        suffix = (query_sep + query if query_sep else "") + (hash_ + fragment if hash_ else "")

        if rel in self.pages:
            if fragment:
                self.anchors.append((src, self.pages[rel], unquote(fragment)))
            return quote(relative_url(self.pages[rel], out)) + suffix
        if rel.startswith("..") or rel == ".":
            self.broken.append((src, href))
            return href
        full = self.root / rel
        if rel.startswith("docs/") and full.is_file():
            # Copied next to the pages (every non-Markdown file under docs/ is).
            return quote(relative_url(rel[len("docs/"):], out)) + suffix
        if full.exists():
            return self.github(rel, raw) + (hash_ + fragment if hash_ else "")
        if rel in self.generated and rel.startswith("docs/") and rel.endswith(".md"):
            # CI writes it; the page will exist on the site even if this checkout lacks it.
            self.missing_generated.add(rel)
            return quote(relative_url(posixpath.splitext(rel[len("docs/"):])[0] + ".html", out)) + suffix
        self.broken.append((src, href))
        return self.github(rel, raw) + (hash_ + fragment if hash_ else "")

    def rewrite_html(self, html: str, src: str, out: str) -> str:
        """Rewrite the href/src attributes inside raw HTML in a Markdown file."""

        def sub(m: re.Match[str]) -> str:
            raw = m.group(1).strip().lower().startswith("src")
            return f"{m.group(1)}{m.group(2)}{self.rewrite(m.group(3), src, out, raw=raw)}{m.group(2)}"

        return HTML_URL.sub(sub, html)
