"""Find the pages of the site and group them into the sidebar's sections."""

from __future__ import annotations

import posixpath
from pathlib import Path

from model import Config, Page

HOME = "index.html"
ASSETS = "site-assets"  # theme files and the search index (theme.py uses the same name)
RESERVED = ("contents.html", "404.html")  # pages the generator writes itself


def md_to_html(rel_to_docs: str) -> str:
    return posixpath.splitext(rel_to_docs)[0] + ".html"


def discover(root: Path, cfg: Config) -> list[Page]:
    """Every Markdown file under docs/ (including CI-generated ones present on
    disk) and the configured extras, each with its output path."""
    pages: list[Page] = []
    docs = root / "docs"
    for path in sorted(docs.rglob("*.md")):
        rel = path.relative_to(root).as_posix()
        pages.append(Page(src=rel, out=md_to_html(path.relative_to(docs).as_posix())))
    for src, out in cfg.extras.items():
        if not (root / src).is_file():
            raise SystemExit(f"site.toml: extra page {src} does not exist")
        pages.append(Page(src=src, out=out, section=cfg.extra_sections.get(src, "")))
    outs: dict[str, str] = {name: "the generator" for name in RESERVED}
    for page in pages:
        if page.out in outs:
            raise SystemExit(f"{page.src} and {outs[page.out]} would both build {page.out}")
        outs[page.out] = page.src
    for path in sorted(docs.rglob("*")):  # files copied beside the pages
        if path.is_file() and path.suffix != ".md":
            rel = path.relative_to(docs).as_posix()
            if rel in outs or rel.startswith(ASSETS + "/"):
                raise SystemExit(f"docs/{rel} is copied to the site but would overwrite {outs.get(rel, 'the theme')}")
    return pages


def section_of(page: Page, cfg: Config) -> str:
    if page.section:
        return page.section
    if page.out == HOME:
        return "Home"
    if page.src in cfg.overview:
        return "Overview"
    parts = page.src.split("/")
    if len(parts) > 2:  # docs/<dir>/...
        directory = "/".join(parts[:2])
        return cfg.directories.get(directory, parts[1].replace("-", " ").replace("_", " ").title())
    stem = posixpath.splitext(parts[-1])[0]
    if any(stem.endswith(s.rstrip("-")) for s in cfg.plan_suffixes):
        return "Plans"
    return "Guides & reference"


def assign_sections(pages: list[Page], cfg: Config) -> None:
    for page in pages:
        page.section = section_of(page, cfg)
    known = {p.src for p in pages}
    for src in cfg.overview:
        if src not in known:
            raise SystemExit(f"site.toml: overview page {src} does not exist")


def build_nav(pages: list[Page], cfg: Config) -> list[tuple[str, list[Page]]]:
    """Sections in the configured order, each page list sorted by title (the
    overview keeps the order the config gives)."""
    groups: dict[str, list[Page]] = {}
    for page in pages:
        if page.out == HOME:
            continue
        groups.setdefault(page.section, []).append(page)
    for name, members in groups.items():
        if name == "Overview":
            members.sort(key=lambda p: cfg.overview.index(p.src))
        else:
            members.sort(key=lambda p: (p.title.lower(), p.src))
    rank = {name: i for i, name in enumerate(cfg.section_order)}
    names = sorted(groups, key=lambda n: (rank.get(n, len(rank)), n.lower()))
    return [(n, groups[n]) for n in names]
