#!/usr/bin/env python3
"""Build the LazyOS documentation site (GitHub Pages) from docs/.

    python tools/site/build.py                 # -> target/site
    python tools/site/build.py --strict        # fail on a broken link (CI)
    python -m http.server -d target/site       # look at it

Every Markdown file under docs/ becomes a page, as do README.md and
CONTRIBUTING.md (site.toml); every other file under docs/ is copied beside the
pages. Links are rewritten for the site, code outside docs/ links to GitHub, and
all page URLs are relative, so the output works under any path (a project site, a
preview, a local server; search needs http, not file://). `tools/site/README.md` has the rest.
"""

from __future__ import annotations

import argparse
import html
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from collect import HOME, assign_sections, build_nav, discover  # noqa: E402
from links import LinkResolver  # noqa: E402
from model import HERE, ROOT, Config, Page, load_config  # noqa: E402
from render import Renderer  # noqa: E402
from theme import ASSETS, highlight_css, render_page  # noqa: E402

INDEX_TEXT_LIMIT = 5000


def git(*args: str, root: Path = ROOT) -> str:
    try:
        return subprocess.run(
            ["git", *args], cwd=root, check=True, capture_output=True, text=True
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return ""


def apply_environment(cfg: Config) -> None:
    """CI knows the repository and the ref better than site.toml does."""
    cfg.repo = os.environ.get("GITHUB_REPOSITORY") or cfg.repo
    cfg.ref = os.environ.get("SITE_REF") or cfg.ref


def default_base_url(cfg: Config) -> str:
    """Where Pages serves the site: /<repo>/, or / for a user site (<owner>.github.io).
    Only 404.html needs it (it is served from any path); a custom domain sets SITE_BASE_URL=/."""
    name = cfg.repo.rsplit("/", 1)[-1]
    return "/" if name.endswith(".github.io") else f"/{name}/"


def render_all(root: Path, cfg: Config, pages: list[Page]) -> LinkResolver:
    resolver = LinkResolver(
        root=root,
        repo_url=cfg.repo_url,
        ref=cfg.ref,
        pages={p.src: p.out for p in pages},
        generated=cfg.generated,
        aliases=cfg.aliases,
    )
    renderer = Renderer(resolver)
    for page in pages:
        done = renderer.render((root / page.src).read_text(encoding="utf-8"), page.src, page.out)
        page.body, page.summary, page.text = done.body, done.summary, done.text
        page.toc, page.ids = done.toc, done.ids
        page.title = cfg.titles.get(page.src) or done.title or Path(page.src).stem.replace("-", " ").title()
    return resolver


def check_anchors(pages: list[Page], resolver: LinkResolver) -> list[str]:
    by_out = {p.out: p for p in pages}
    problems = []
    for src, target, fragment in resolver.anchors:
        page = by_out[target]
        if fragment not in page.ids:
            problems.append(f"{src}: no heading #{fragment} in {page.src}")
    return sorted(set(problems))


def contents_page(nav: list[tuple[str, list[Page]]]) -> Page:
    """An index of every document with its first sentence."""
    parts = ["<h1>All documents</h1>", '<div class="contents">']
    for name, members in nav:
        parts.append(f"<section><h2>{html.escape(name)}</h2><dl>")
        for page in members:
            parts.append(f'<dt><a href="{html.escape(page.out)}">{html.escape(page.title)}</a></dt>')
            if page.summary:
                parts.append(f"<dd>{html.escape(page.summary)}</dd>")
        parts.append("</dl></section>")
    parts.append("</div>")
    return Page(src="", out="contents.html", title="All documents", body="\n".join(parts))


def search_index(pages: list[Page]) -> list[dict[str, str]]:
    return [
        {
            "t": p.title,
            "u": p.out,
            "g": p.section,
            "s": p.summary,
            "h": " | ".join(t for _, _, t in p.toc),
            "x": p.text[:INDEX_TEXT_LIMIT],
        }
        for p in sorted(pages, key=lambda p: p.out)
    ]


def copy_docs_files(root: Path, out: Path) -> int:
    """Everything under docs/ that is not a page (text, logs, HTML, binaries)."""
    count = 0
    for path in sorted((root / "docs").rglob("*")):
        if path.is_file() and path.suffix != ".md":
            dest = out / path.relative_to(root / "docs")
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, dest)
            count += 1
    return count


def write_assets(out: Path) -> None:
    target = out / ASSETS
    target.mkdir(parents=True, exist_ok=True)
    shutil.copy2(HERE / "assets" / "site.js", target / "site.js")
    css = (HERE / "assets" / "style.css").read_text(encoding="utf-8")
    (target / "style.css").write_text(css + "\n" + highlight_css(), encoding="utf-8")


def build(root: Path, out: Path, cfg: Config, base_url: str, commit: str) -> tuple[list[Page], LinkResolver]:
    pages = discover(root, cfg)
    assign_sections(pages, cfg)
    resolver = render_all(root, cfg, pages)
    nav = build_nav(pages, cfg)
    toc_page = contents_page(nav)

    if out.exists():
        shutil.rmtree(out)
    out.mkdir(parents=True)
    for page in [*pages, toc_page]:
        # No source to edit for the contents page or for what CI generates.
        edit = f"{cfg.repo_url}/blob/{cfg.ref}/{page.src}" if page.src and page.src not in cfg.generated else ""
        html_text = render_page(page, cfg, nav, commit, edit_url=edit)
        dest = out / page.out
        dest.parent.mkdir(parents=True, exist_ok=True)
        dest.write_text(html_text, encoding="utf-8")

    missing = Page(
        src="", out="404.html", title="Page not found", section="",
        body=f'<h1>Page not found</h1><p>That page is not in the docs. '
        f'Try the <a href="{base_url}index.html">home page</a> or '
        f'<a href="{base_url}contents.html">the list of all documents</a>.</p>',
    )
    (out / "404.html").write_text(render_page(missing, cfg, nav, commit, base_href=base_url), encoding="utf-8")
    (out / ".nojekyll").write_text("", encoding="utf-8")
    write_assets(out)
    (out / ASSETS / "search-index.json").write_text(
        json.dumps(search_index(pages), separators=(",", ":")), encoding="utf-8"
    )
    files = copy_docs_files(root, out)
    print(f"site: {len(pages)} pages, {files} copied files -> {out}")
    return pages, resolver


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", type=Path, default=ROOT / "target" / "site", help="output directory (replaced)")
    ap.add_argument("--root", type=Path, default=ROOT, help="repository root")
    ap.add_argument("--config", type=Path, default=HERE / "site.toml", help="site configuration")
    ap.add_argument("--strict", action="store_true", help="fail on a broken link or a bad fragment")
    ap.add_argument("--base-url", help="URL path the site is served under, for 404.html only "
                    "(default /<repo name>/, or SITE_BASE_URL)")
    args = ap.parse_args(argv)

    cfg = load_config(args.config)
    apply_environment(cfg)
    base_url = args.base_url or os.environ.get("SITE_BASE_URL") or default_base_url(cfg)
    commit = (os.environ.get("GITHUB_SHA") or git("rev-parse", "HEAD", root=args.root) or "unknown")[:10]
    pages, resolver = build(args.root, args.out, cfg, base_url, commit)

    for rel in sorted(resolver.missing_generated):
        print(f"site: note: {rel} is generated by CI and absent here")
    bad_anchors = check_anchors(pages, resolver)
    for src, href in sorted(set(resolver.broken)):
        print(f"site: broken link in {src}: {href}")
    for line in bad_anchors:
        print(f"site: bad fragment: {line}")
    problems = len(set(resolver.broken)) + len(bad_anchors)
    if problems:
        print(f"site: {problems} link problem(s)")
    return 1 if (problems and args.strict) else 0


if __name__ == "__main__":
    raise SystemExit(main())
