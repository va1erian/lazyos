"""Markdown to HTML for one page (CommonMark plus GitHub's tables, task lists,
strikethrough, footnotes and autolinks), with highlighted code and GitHub's
heading anchors so a `#fragment` written for GitHub still lands."""

from __future__ import annotations

import html
import re
from dataclasses import dataclass, field

from markdown_it import MarkdownIt
from markdown_it.token import Token
from mdit_py_plugins.anchors import anchors_plugin
from mdit_py_plugins.footnote import footnote_plugin
from mdit_py_plugins.tasklists import tasklists_plugin
from pygments import highlight as pygments_highlight
from pygments.formatters import HtmlFormatter
from pygments.lexers import get_lexer_by_name
from pygments.util import ClassNotFound

from links import LinkResolver

# Fence languages pygments does not know, mapped to a close lexer.
LANG_ALIASES = {"rhai": "rust", "console": "text", "midl": "text", "lzp": "text"}
FORMATTER = HtmlFormatter(nowrap=True)


@dataclass
class Rendered:
    body: str
    title: str = ""
    summary: str = ""
    text: str = ""
    toc: list[tuple[int, str, str]] = field(default_factory=list)
    ids: set[str] = field(default_factory=set)


def github_slug(text: str) -> str:
    """GitHub's heading id: lower case, punctuation dropped, spaces to hyphens."""
    return re.sub(r"[^\w\- ]", "", text.strip().lower()).replace(" ", "-")


def highlight(code: str, lang: str, _attrs: str) -> str:
    name = LANG_ALIASES.get(lang.lower(), lang.lower()) if lang else ""
    if not name or name == "text":
        return ""
    try:
        lexer = get_lexer_by_name(name)
    except ClassNotFound:
        return ""
    body = pygments_highlight(code, lexer, FORMATTER)
    return f'<pre class="highlight"><code class="language-{html.escape(lang)}">{body}</code></pre>\n'


def make_parser() -> MarkdownIt:
    md = MarkdownIt("commonmark", {"html": True, "linkify": True, "highlight": highlight})
    md.enable(["table", "strikethrough", "linkify"])
    md.use(anchors_plugin, min_level=1, max_level=4, slug_func=github_slug)
    md.use(footnote_plugin)
    md.use(tasklists_plugin)

    def table_open(self, tokens, idx, options, env):  # noqa: ANN001
        return '<div class="table-wrap">' + self.renderToken(tokens, idx, options, env)

    def table_close(self, tokens, idx, options, env):  # noqa: ANN001
        return self.renderToken(tokens, idx, options, env) + "</div>\n"

    def heading_close(self, tokens, idx, options, env):  # noqa: ANN001
        ident = tokens[idx - 2].attrGet("id")
        link = f'<a class="anchor" href="#{ident}" aria-label="Link to this section">#</a>' if ident else ""
        return link + self.renderToken(tokens, idx, options, env)

    md.add_render_rule("table_open", table_open)
    md.add_render_rule("table_close", table_close)
    md.add_render_rule("heading_close", heading_close)
    return md


def inline_text(token: Token) -> str:
    """The words of an inline token, without markup."""
    parts = []
    for child in token.children or []:
        if child.type in ("text", "code_inline"):
            parts.append(child.content)
        elif child.type in ("softbreak", "hardbreak"):
            parts.append(" ")
    return "".join(parts)


class Renderer:
    def __init__(self, resolver: LinkResolver) -> None:
        self.md = make_parser()
        self.resolver = resolver

    def render(self, source: str, src: str, out: str) -> Rendered:
        env: dict = {}
        tokens = self.md.parse(source, env)
        result = Rendered(body="")
        words: list[str] = []
        for i, token in enumerate(tokens):
            if token.type == "html_block":
                token.content = self.resolver.rewrite_html(token.content, src, out)
            elif token.type == "inline":
                self._inline(token, src, out)
                if tokens[i - 1].type == "heading_open":
                    self._heading(tokens[i - 1], token, result)
                text = inline_text(token)
                words.append(text)
                if not result.summary and tokens[i - 1].type == "paragraph_open" and len(text) > 30:
                    result.summary = text[:220].rstrip() + ("…" if len(text) > 220 else "")
        result.text = " ".join(words)
        result.body = self.md.renderer.render(tokens, self.md.options, env)
        return result

    def _inline(self, token: Token, src: str, out: str) -> None:
        for child in token.children or []:
            if child.type == "link_open":
                child.attrSet("href", self.resolver.rewrite(child.attrGet("href") or "", src, out))
            elif child.type == "image":
                child.attrSet("src", self.resolver.rewrite(child.attrGet("src") or "", src, out))
            elif child.type == "html_inline":
                child.content = self.resolver.rewrite_html(child.content, src, out)

    @staticmethod
    def _heading(opener: Token, inline: Token, result: Rendered) -> None:
        text = inline_text(inline)
        ident = opener.attrGet("id") or ""
        if ident:
            result.ids.add(ident)
        level = int(opener.tag[1])
        if level == 1 and not result.title:
            result.title = text
        elif level in (2, 3) and ident:
            result.toc.append((level, ident, text))
