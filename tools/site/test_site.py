"""Tests for tools/site (offline, temp trees): link rewriting, anchors, nav and
a whole build of a miniature repository."""

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import build as site_build  # noqa: E402
from collect import assign_sections, build_nav, discover  # noqa: E402
from links import LinkResolver, relative_url  # noqa: E402
from model import Config, load_config  # noqa: E402
from render import Renderer, github_slug  # noqa: E402


def config(**over) -> Config:
    base = dict(
        title="Test", tagline="t", repo="o/r", ref="main", section_order=["Overview", "Plans"],
        overview=[], directories={"docs/sub": "Sub pages"}, plan_suffixes=["-plan"],
        extras={"README.md": "index.html"}, extra_sections={}, generated={"docs/compat/matrix.md"},
    )
    base.update(over)
    return Config(**base)


class TreeCase(unittest.TestCase):
    def setUp(self) -> None:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name) / "repo"
        self.out = Path(tmp.name) / "site"

    def put(self, name: str, text: str = "x") -> None:
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def build(self, cfg: Config | None = None):
        return site_build.build(self.root, self.out, cfg or config(), "/r/", "abc123")


class LinkTests(TreeCase):
    def resolver(self) -> LinkResolver:
        self.put("docs/a.md")
        self.put("docs/sub/b.md")
        self.put("docs/data.txt")
        self.put("kernel/src/lib.rs")
        return LinkResolver(
            self.root, "https://github.com/o/r", "main",
            {"docs/a.md": "a.html", "docs/sub/b.md": "sub/b.html"},
            generated={"docs/compat/matrix.md"},
        )

    def test_page_links_become_html_and_keep_fragments(self) -> None:
        r = self.resolver()
        self.assertEqual(r.rewrite("sub/b.md#x", "docs/a.md", "a.html"), "sub/b.html#x")
        self.assertEqual(r.rewrite("../a.md", "docs/sub/b.md", "sub/b.html"), "../a.html")
        self.assertEqual(r.anchors[0], ("docs/a.md", "sub/b.html", "x"))

    def test_code_outside_docs_goes_to_github(self) -> None:
        r = self.resolver()
        self.assertEqual(
            r.rewrite("../kernel/src/lib.rs", "docs/a.md", "a.html"),
            "https://github.com/o/r/blob/main/kernel/src/lib.rs",
        )
        self.assertEqual(
            r.rewrite("../kernel/src", "docs/a.md", "a.html"),
            "https://github.com/o/r/tree/main/kernel/src",
        )

    def test_docs_files_that_are_not_pages_are_copied_links(self) -> None:
        r = self.resolver()
        self.assertEqual(r.rewrite("../data.txt", "docs/sub/b.md", "sub/b.html"), "../data.txt")

    def test_untouched_links(self) -> None:
        r = self.resolver()
        for href in ("https://x.org/a.md", "mailto:a@b.c", "#frag", "//cdn/x.js", ""):
            self.assertEqual(r.rewrite(href, "docs/a.md", "a.html"), href)
        self.assertEqual(r.broken, [])

    def test_missing_target_is_broken_but_generated_is_not(self) -> None:
        r = self.resolver()
        r.rewrite("nope.md", "docs/a.md", "a.html")
        r.rewrite("compat/matrix.md", "docs/a.md", "a.html")
        self.assertEqual(r.broken, [("docs/a.md", "nope.md")])
        self.assertEqual(r.missing_generated, {"docs/compat/matrix.md"})

    def test_link_that_leaves_the_repository_is_broken(self) -> None:
        r = self.resolver()
        r.rewrite("../../../etc/passwd", "docs/a.md", "a.html")
        self.assertEqual(len(r.broken), 1)

    def test_html_attributes_are_rewritten(self) -> None:
        r = self.resolver()
        out = r.rewrite_html('<a href="sub/b.md">b</a> <img src=\'data.txt\'>', "docs/a.md", "a.html")
        self.assertIn('href="sub/b.html"', out)
        self.assertIn("src='data.txt'", out)

    def test_images_outside_docs_use_raw_urls(self) -> None:
        r = self.resolver()
        self.assertEqual(
            r.rewrite("../kernel/src/lib.rs", "docs/a.md", "a.html", raw=True),
            "https://github.com/o/r/raw/main/kernel/src/lib.rs",
        )
        out = r.rewrite_html('<img src="../kernel/src/lib.rs"> <a href="../kernel/src/lib.rs">k</a>', "docs/a.md", "a.html")
        self.assertIn("/raw/main/", out)
        self.assertIn("/blob/main/", out)

    def test_same_page_fragments_are_recorded_and_decoded(self) -> None:
        r = self.resolver()
        self.assertEqual(r.rewrite("#sec%C3%A9", "docs/a.md", "a.html"), "#sec%C3%A9")
        r.rewrite("sub/b.md#caf%C3%A9", "docs/a.md", "a.html")
        self.assertEqual(r.anchors, [("docs/a.md", "a.html", "secé"), ("docs/a.md", "sub/b.html", "café")])

    def test_generated_page_missing_locally_links_to_its_site_page(self) -> None:
        r = self.resolver()
        self.assertEqual(r.rewrite("compat/matrix.md", "docs/a.md", "a.html"), "compat/matrix.html")

    def test_odd_file_names_are_percent_encoded(self) -> None:
        self.put("docs/odd name#1.txt")
        r = self.resolver()
        self.assertEqual(r.rewrite("odd%20name%231.txt", "docs/a.md", "a.html"), "odd%20name%231.txt")

    def test_relative_url(self) -> None:
        self.assertEqual(relative_url("a.html", "a.html"), "a.html")
        self.assertEqual(relative_url("x/y.html", "a.html"), "x/y.html")
        self.assertEqual(relative_url("a.html", "x/y/z.html"), "../../a.html")


class RenderTests(TreeCase):
    def render(self, text: str):
        r = LinkResolver(self.root, "https://github.com/o/r", "main", {})
        return Renderer(r).render(text, "docs/a.md", "a.html")

    def test_github_slugs(self) -> None:
        self.assertEqual(github_slug("What it IS: a `thing` (v2)"), "what-it-is-a-thing-v2")
        self.assertEqual(github_slug("A_b - c"), "a_b---c")

    def test_duplicate_headings_get_github_suffixes(self) -> None:
        done = self.render("# T\n\n## Same\n\n## Same\n")
        self.assertEqual(done.ids, {"t", "same", "same-1"})

    def test_title_toc_and_summary(self) -> None:
        done = self.render("# Title `x`\n\nOne paragraph that is long enough to summarise.\n\n## A\n\n### B\n")
        self.assertEqual(done.title, "Title x")
        self.assertEqual([(lv, i) for lv, i, _ in done.toc], [(2, "a"), (3, "b")])
        self.assertTrue(done.summary.startswith("One paragraph"))

    def test_github_flavoured_features(self) -> None:
        done = self.render("|a|b|\n|-|-|\n|1|2|\n\n~~x~~ https://example.org\n\n- [x] done\n\n```rust\nfn f() {}\n```\n")
        for needle in ("table-wrap", "<s>x</s>", 'href="https://example.org"', "task-list-item", 'class="highlight"'):
            self.assertIn(needle, done.body)

    def test_unknown_fence_language_is_plain(self) -> None:
        done = self.render("```midl\ninterface x {}\n```\n")
        self.assertIn("<pre><code", done.body)

    def test_bare_file_names_are_not_autolinked(self) -> None:
        body = self.render("See AGENTS.md, lib.rs and tools/x.py, or https://example.org.\n").body
        self.assertNotIn("http://AGENTS.md", body)
        self.assertNotIn("<a href=\"http://lib.rs", body)
        self.assertIn('href="https://example.org"', body)

    def test_deep_headings_get_ids(self) -> None:
        self.assertIn("deep", self.render("# T\n\n###### Deep\n").ids)

    def test_raw_html_is_kept(self) -> None:
        self.assertIn("<details>", self.render("<details><summary>s</summary>\n\nbody\n\n</details>\n").body)


class NavTests(TreeCase):
    def test_sections(self) -> None:
        for name in ("docs/guide.md", "docs/x-plan.md", "docs/sub/b.md", "docs/lonely/c.md", "README.md"):
            self.put(name, "# " + name + "\n")
        cfg = config(overview=["docs/guide.md"])
        pages = discover(self.root, cfg)
        assign_sections(pages, cfg)
        for p in pages:
            p.title = p.src
        got = {p.src: p.section for p in pages}
        self.assertEqual(got["docs/guide.md"], "Overview")
        self.assertEqual(got["docs/x-plan.md"], "Plans")
        self.assertEqual(got["docs/sub/b.md"], "Sub pages")
        self.assertEqual(got["docs/lonely/c.md"], "Lonely")
        self.assertEqual(got["README.md"], "Home")
        names = [n for n, _ in build_nav(pages, cfg)]
        self.assertEqual(names, ["Overview", "Plans", "Lonely", "Sub pages"])

    def test_missing_overview_page_is_an_error(self) -> None:
        self.put("docs/a.md")
        cfg = config(overview=["docs/gone.md"], extras={})
        pages = discover(self.root, cfg)
        with self.assertRaises(SystemExit):
            assign_sections(pages, cfg)

    def test_colliding_output_paths_are_an_error(self) -> None:
        self.put("docs/index.md")
        self.put("README.md")
        with self.assertRaises(SystemExit):
            discover(self.root, config())


class BuildTests(TreeCase):
    def mini(self) -> None:
        self.put("README.md", "# Home\n\nSee [guide](docs/guide.md) and [code](kernel/x.rs).\n")
        self.put("kernel/x.rs", "fn main() {}\n")
        self.put("docs/guide.md", "# Guide\n\nText with a [fragment](sub/b.md#deep) and [gone](gone.md).\n")
        self.put("docs/sub/b.md", "# B\n\n## Deep\n\nbody\n")
        self.put("docs/raw/notes.txt", "plain\n")

    def test_whole_site(self) -> None:
        self.mini()
        pages, resolver = self.build()
        self.assertEqual([("docs/guide.md", "gone.md")], resolver.broken)
        for rel in ("index.html", "guide.html", "sub/b.html", "contents.html", "404.html", ".nojekyll",
                    "raw/notes.txt", "site-assets/style.css", "site-assets/site.js"):
            self.assertTrue((self.out / rel).is_file(), rel)
        index = (self.out / "index.html").read_text(encoding="utf-8")
        self.assertIn('href="guide.html"', index)
        self.assertIn("https://github.com/o/r/blob/main/kernel/x.rs", index)
        nested = (self.out / "sub" / "b.html").read_text(encoding="utf-8")
        self.assertIn('href="../site-assets/style.css"', nested)
        self.assertIn('href="../guide.html"', nested)
        self.assertIn("Built from commit <code>abc123</code>", nested)

    def test_urls_are_relative_so_the_site_works_under_any_path(self) -> None:
        self.mini()
        self.build()
        for page in self.out.rglob("*.html"):
            if page.name == "404.html":
                continue
            text = page.read_text(encoding="utf-8")
            self.assertNotIn('href="/', text, page.name)
            self.assertNotIn('src="/', text, page.name)

    def test_search_index(self) -> None:
        self.mini()
        self.build()
        index = json.loads((self.out / "site-assets" / "search-index.json").read_text(encoding="utf-8"))
        deep = next(d for d in index if d["u"] == "sub/b.html")
        self.assertEqual((deep["t"], deep["h"]), ("B", "Deep"))

    def test_rebuild_replaces_the_output_and_is_deterministic(self) -> None:
        self.mini()
        self.build()
        (self.out / "stale.html").write_text("old", encoding="utf-8")
        self.build()
        self.assertFalse((self.out / "stale.html").exists())
        first = (self.out / "guide.html").read_bytes()
        self.build()
        self.assertEqual(first, (self.out / "guide.html").read_bytes())

    def test_bad_fragment_is_reported(self) -> None:
        self.mini()
        self.put("docs/guide.md", "# Guide\n\n[x](sub/b.md#nothing-here)\n")
        pages, resolver = self.build()
        self.assertEqual(len(site_build.check_anchors(pages, resolver)), 1)

    def test_docs_file_may_not_overwrite_a_generated_or_rendered_page(self) -> None:
        self.put("README.md", "# Home\n")
        for clash in ("docs/contents.md", "docs/404.md"):
            self.put(clash, "# X\n")
            with self.assertRaises(SystemExit, msg=clash):
                self.build()
            (self.root / clash).unlink()
        self.put("docs/a.md", "# A\n")
        self.put("docs/a.html", "<p>hand-written</p>")
        with self.assertRaises(SystemExit):
            self.build()

    def test_generated_pages_have_no_edit_link(self) -> None:
        self.put("README.md", "# Home\n")
        self.put("docs/compat/matrix.md", "# Matrix\n")
        cfg = config(generated={"docs/compat/matrix.md"})
        self.build(cfg)
        self.assertNotIn("Edit this page", (self.out / "compat" / "matrix.html").read_text(encoding="utf-8"))
        self.assertIn("Edit this page", (self.out / "index.html").read_text(encoding="utf-8"))

    def test_default_base_url(self) -> None:
        self.assertEqual(site_build.default_base_url(config(repo="o/lazyos")), "/lazyos/")
        self.assertEqual(site_build.default_base_url(config(repo="o/o.github.io")), "/")

    def test_titles_are_escaped(self) -> None:
        self.put("README.md", "# Home\n")
        self.put("docs/a.md", "# A &lt;b&gt; co\n\nA paragraph about &lt;i&gt; things that is long enough.\n")
        self.build()
        contents = (self.out / "contents.html").read_text(encoding="utf-8")
        self.assertIn("A &lt;b&gt; co", contents)
        self.assertNotIn("<b>", contents)
        self.assertNotIn("<i>", contents)

    def test_cli_strict_fails_on_broken_link(self) -> None:
        self.mini()
        toml = Path(self.root).parent / "site.toml"
        toml.write_text(
            '[site]\ntitle="T"\ntagline="t"\nrepo="o/r"\nref="main"\n'
            '[nav]\norder=[]\noverview=[]\n[nav.directories]\n[nav.plans]\nsuffixes=["-plan"]\n'
            '[extras]\n"README.md"="index.html"\n[generated]\npaths=[]\n',
            encoding="utf-8",
        )
        args = ["--config", str(toml), "--root", str(self.root), "--out", str(self.out)]
        self.assertEqual(site_build.main(args), 0)
        self.assertEqual(site_build.main([*args, "--strict"]), 1)


class RealConfigTests(unittest.TestCase):
    def test_site_toml_loads_and_names_real_pages(self) -> None:
        cfg = load_config()
        root = Path(__file__).resolve().parents[2]
        for rel in [*cfg.overview, *cfg.extras]:
            self.assertTrue((root / rel).is_file(), rel)


if __name__ == "__main__":
    unittest.main()
