"""The HTML around a page: header, sidebar, contents column, footer."""

from __future__ import annotations

import html

from links import relative_url
from model import Config, Page
from pygments.formatters import HtmlFormatter

ASSETS = "site-assets"

# Applied before the page paints so a stored choice does not flash.
THEME_BOOT = (
    "try{var t=localStorage.getItem('lazyos-docs-theme');"
    "if(t==='light'||t==='dark'){document.documentElement.setAttribute('data-theme',t)}}catch(e){}"
)


FAVICON = (
    "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'%3E"
    "%3Crect width='16' height='16' rx='3' fill='%236639ba'/%3E"
    "%3Cpath d='M4 3v8h6v2H2V3z' fill='white'/%3E%3C/svg%3E"
)


def highlight_css() -> str:
    """Code colours for both themes, scoped like the page's own theming."""
    light = HtmlFormatter(style="default").get_style_defs(".highlight")
    dark = HtmlFormatter(style="github-dark").get_style_defs(".highlight")
    auto = dark.replace(".highlight", ':root:not([data-theme="light"]) .highlight')
    forced = dark.replace(".highlight", ':root[data-theme="dark"] .highlight')
    return f"{light}\n@media (prefers-color-scheme: dark) {{\n{auto}\n}}\n{forced}\n"


def esc(text: str) -> str:
    return html.escape(text, quote=True)


def sidebar(nav: list[tuple[str, list[Page]]], current: Page, depth_from: str) -> str:
    out = ['<nav class="sidebar" aria-label="Documentation">']
    for name, members in nav:
        opened = name == "Overview" or any(p.out == current.out for p in members)
        out.append(f'<details{" open" if opened else ""}><summary>{esc(name)}</summary><ul>')
        for page in members:
            here = ' aria-current="page"' if page.out == current.out else ""
            out.append(f'<li><a href="{esc(relative_url(page.out, depth_from))}"{here}>{esc(page.title)}</a></li>')
        out.append("</ul></details>")
    out.append("</nav>")
    return "\n".join(out)


def toc_column(page: Page) -> str:
    if len(page.toc) < 2:
        return '<aside class="toc"></aside>'
    items = "".join(
        f'<li class="l{level}"><a href="#{esc(ident)}">{esc(text)}</a></li>' for level, ident, text in page.toc
    )
    return f'<aside class="toc" aria-label="On this page"><h2>On this page</h2><ul>{items}</ul></aside>'


def render_page(
    page: Page,
    cfg: Config,
    nav: list[tuple[str, list[Page]]],
    commit: str,
    *,
    base_href: str = "",
    edit_url: str = "",
) -> str:
    """A complete HTML document. `page.out` decides every relative URL; a page
    with `base_href` (the 404 page) uses paths from that base instead."""
    at = "" if base_href else page.out
    prefix = "" if base_href else relative_url(".", at) + "/"
    prefix = "" if prefix == "./" else prefix
    asset = lambda name: f"{prefix}{ASSETS}/{name}"  # noqa: E731
    home = f"{prefix}index.html" if prefix else "index.html"
    contents = f"{prefix}contents.html" if prefix else "contents.html"
    title = page.title if page.out == "index.html" else f"{page.title} · {cfg.title}"
    base = f'<base href="{esc(base_href)}">' if base_href else ""
    edit = (
        f'<a href="{esc(edit_url)}">Edit this page on GitHub</a> · '
        if edit_url
        else ""
    )
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
{base}
<title>{esc(title)}</title>
<meta name="description" content="{esc(page.summary or cfg.tagline)}">
<script>{THEME_BOOT}</script>
<link rel="icon" href="{FAVICON}">
<link rel="stylesheet" href="{asset("style.css")}">
</head>
<body>
<header class="site-header">
<button class="icon-btn" id="menu-toggle" aria-label="Toggle navigation" aria-expanded="false">Menu</button>
<a class="brand" href="{home}">{esc(cfg.title)}</a>
<div class="search-box">
<input id="search" type="search" placeholder="Search the docs (press /)" aria-label="Search the docs" autocomplete="off">
<ul id="search-results" hidden></ul>
</div>
<nav class="top">
<a class="hide-sm" href="{contents}">All documents</a>
<a class="hide-sm" href="{esc(cfg.repo_url)}">GitHub</a>
<button class="icon-btn" id="theme-toggle" aria-label="Toggle light and dark theme">Theme</button>
</nav>
</header>
<div class="layout">
{sidebar(nav, page, at or ".") if not base_href else '<nav class="sidebar"></nav>'}
<main><article>
{page.body}
<div class="page-meta">{edit}Built from commit <code>{esc(commit)}</code></div>
</article></main>
{toc_column(page)}
</div>
<script src="{asset("site.js")}" defer></script>
</body>
</html>
"""
