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
from urllib.parse import unquote

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

    def github(self, rel: str) -> str:
        kind = "tree" if (self.root / rel).is_dir() else "blob"
        return f"{self.repo_url}/{kind}/{self.ref}/{rel}"

    def rewrite(self, href: str, src: str, out: str) -> str:
        """The URL to use for `href`, written in `src` and built to `out`."""
        if not href or href.startswith("#") or SCHEME.match(href):
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
                self.anchors.append((src, self.pages[rel], fragment))
            return relative_url(self.pages[rel], out) + suffix
        if rel.startswith("..") or rel == ".":
            self.broken.append((src, href))
            return href
        full = self.root / rel
        if rel.startswith("docs/") and full.is_file():
            # Copied next to the pages (every non-Markdown file under docs/ is).
            return relative_url(rel[len("docs/"):], out) + suffix
        if full.exists():
            return self.github(rel) + (hash_ + fragment if hash_ else "")
        if rel in self.generated:
            self.missing_generated.add(rel)
            return href
        self.broken.append((src, href))
        return self.github(rel) + (hash_ + fragment if hash_ else "")

    def rewrite_html(self, html: str, src: str, out: str) -> str:
        """Rewrite the href/src attributes inside raw HTML in a Markdown file."""

        def sub(m: re.Match[str]) -> str:
            return f"{m.group(1)}{m.group(2)}{self.rewrite(m.group(3), src, out)}{m.group(2)}"

        return HTML_URL.sub(sub, html)
