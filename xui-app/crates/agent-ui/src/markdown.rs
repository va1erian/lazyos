//! Markdown to blocks (docs/ai-agent-plan.md §9.1). Pure: no xui type, run on
//! the layout thread. A model's reply is untrusted text, so what is drawn is
//! a small subset: paragraphs, headings, list items, block quotes, fenced
//! code and rules. Emphasis and inline code lose their markers (the shaper
//! styles a whole block, not a run), raw HTML is dropped, images become their
//! alt text, and a link is kept only when it is `http` or `https`.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

/// The most blocks one message makes; the rest is dropped with a note.
pub const MAX_BLOCKS: usize = 4000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    /// 1 to 3 (deeper headings count as 3).
    Heading(u8),
    /// A fenced or indented code block; the language, if given.
    Code(String),
    /// A list item: its marker (`•` or `1.`) and nesting depth.
    Item {
        marker: String,
        depth: u8,
    },
    Quote,
    Rule,
}

/// A link inside a block's text, as byte offsets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub start: usize,
    pub end: usize,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub kind: BlockKind,
    pub text: String,
    pub links: Vec<Link>,
}

struct Builder {
    blocks: Vec<Block>,
    text: String,
    links: Vec<Link>,
    open_link: Option<(usize, String)>,
    /// One entry per open list: `None` bullets, `Some(n)` the next number.
    lists: Vec<Option<u64>>,
    quote: u32,
    item_marker: Option<String>,
    heading: Option<u8>,
    code: Option<String>,
    dropped: bool,
}

impl Builder {
    fn flush(&mut self) {
        let text = std::mem::take(&mut self.text);
        let links = std::mem::take(&mut self.links);
        self.open_link = None;
        if text.trim().is_empty() {
            self.item_marker = None;
            return;
        }
        let kind = if let Some(marker) = self.item_marker.take() {
            BlockKind::Item {
                marker,
                depth: self.lists.len().saturating_sub(1).min(4) as u8,
            }
        } else if let Some(level) = self.heading {
            BlockKind::Heading(level)
        } else if self.quote > 0 {
            BlockKind::Quote
        } else {
            BlockKind::Paragraph
        };
        self.push(Block { kind, text, links });
    }

    fn push(&mut self, block: Block) {
        if self.blocks.len() >= MAX_BLOCKS {
            self.dropped = true;
        } else {
            self.blocks.push(block);
        }
    }

    fn text(&mut self, text: &str) {
        self.text.push_str(text);
    }
}

fn level(heading: HeadingLevel) -> u8 {
    match heading {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        _ => 3,
    }
}

/// Parses `source` into blocks.
pub fn parse(source: &str) -> Vec<Block> {
    let mut b = Builder {
        blocks: Vec::new(),
        text: String::new(),
        links: Vec::new(),
        open_link: None,
        lists: Vec::new(),
        quote: 0,
        item_marker: None,
        heading: None,
        code: None,
        dropped: false,
    };
    let mut code_text = String::new();
    for event in Parser::new_ext(source, Options::empty()) {
        match event {
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => b.flush(),
            Event::Start(Tag::Heading { level: l, .. }) => {
                b.flush();
                b.heading = Some(level(l));
            }
            Event::End(TagEnd::Heading(_)) => {
                b.flush();
                b.heading = None;
            }
            Event::Start(Tag::BlockQuote(_)) => {
                b.flush();
                b.quote += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                b.flush();
                b.quote = b.quote.saturating_sub(1);
            }
            Event::Start(Tag::List(first)) => {
                b.flush();
                b.lists.push(first);
            }
            Event::End(TagEnd::List(_)) => {
                b.flush();
                b.lists.pop();
            }
            Event::Start(Tag::Item) => {
                b.flush();
                let marker = match b.lists.last_mut() {
                    Some(Some(n)) => {
                        let marker = format!("{n}.");
                        *n += 1;
                        marker
                    }
                    _ => "\u{2022}".to_string(),
                };
                b.item_marker = Some(marker);
            }
            Event::End(TagEnd::Item) => b.flush(),
            Event::Start(Tag::CodeBlock(kind)) => {
                b.flush();
                code_text.clear();
                b.code = Some(match kind {
                    CodeBlockKind::Fenced(lang) => lang
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .filter(|c| c.is_alphanumeric() || "+-_#.".contains(*c))
                        .take(24)
                        .collect(),
                    CodeBlockKind::Indented => String::new(),
                });
            }
            Event::End(TagEnd::CodeBlock) => {
                let lang = b.code.take().unwrap_or_default();
                let text = std::mem::take(&mut code_text);
                b.push(Block {
                    kind: BlockKind::Code(lang),
                    text: text.trim_end_matches('\n').to_string(),
                    links: Vec::new(),
                });
            }
            Event::Start(Tag::Link { dest_url, .. }) => {
                let url = dest_url.to_string();
                if is_web(&url) {
                    b.open_link = Some((b.text.len(), url));
                }
            }
            Event::End(TagEnd::Link) => {
                if let Some((start, url)) = b.open_link.take() {
                    if b.text.len() > start {
                        let end = b.text.len();
                        b.links.push(Link { start, end, url });
                    }
                }
            }
            Event::Text(text) => {
                if b.code.is_some() {
                    code_text.push_str(&text);
                } else {
                    b.text(&text);
                }
            }
            Event::Code(text) => b.text(&text),
            Event::SoftBreak => b.text(" "),
            Event::HardBreak => b.text("\n"),
            Event::Rule => {
                b.flush();
                b.push(Block {
                    kind: BlockKind::Rule,
                    text: String::new(),
                    links: Vec::new(),
                });
            }
            // Raw HTML is never interpreted: it shows as the text it is.
            // Images are their alt text (Text events).
            Event::Html(html) => {
                // Text already in the block ends it; a list item's marker stays.
                if !b.text.trim().is_empty() {
                    b.flush();
                }
                b.text(html.trim_end());
                b.flush();
            }
            Event::InlineHtml(html) => b.text(&html),
            _ => {}
        }
    }
    b.flush();
    if b.dropped {
        b.blocks.push(Block {
            kind: BlockKind::Paragraph,
            text: "[The rest of this message is not shown: it has too many blocks.]".into(),
            links: Vec::new(),
        });
    }
    b.blocks
}

/// The longest address a link may have (the launcher refuses more).
const MAX_URL: usize = 1000;

/// Only web links are followed (and only after the user confirms them).
/// The address is plain ASCII: a bidirectional control or a look-alike letter
/// would make the confirmation show another address than the one opened.
pub fn is_web(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && url.bytes().all(|c| c.is_ascii_graphic())
        && url.len() <= MAX_URL
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(blocks: &[Block]) -> Vec<&BlockKind> {
        blocks.iter().map(|b| &b.kind).collect()
    }

    #[test]
    fn headings_paragraphs_and_lists() {
        let blocks = parse(
            "# Title\n\nSome *emphasis* and `code`.\n\n- one\n- two\n\n1. first\n2. second\n",
        );
        assert_eq!(blocks[0].kind, BlockKind::Heading(1));
        assert_eq!(blocks[1].text, "Some emphasis and code.");
        assert_eq!(
            blocks[2].kind,
            BlockKind::Item {
                marker: "\u{2022}".into(),
                depth: 0
            }
        );
        assert_eq!(
            blocks[4].kind,
            BlockKind::Item {
                marker: "1.".into(),
                depth: 0
            }
        );
        assert_eq!(blocks[5].text, "second");
        assert_eq!(blocks.len(), 6);
    }

    #[test]
    fn nested_lists_and_quotes() {
        let blocks = parse("- a\n  - b\n\n> quoted\n> text\n");
        assert_eq!(blocks[0].text, "a");
        assert_eq!(
            blocks[1].kind,
            BlockKind::Item {
                marker: "\u{2022}".into(),
                depth: 1
            }
        );
        assert_eq!(blocks[2].kind, BlockKind::Quote);
        assert_eq!(blocks[2].text, "quoted text");
    }

    #[test]
    fn fenced_code_keeps_its_text_and_language() {
        let blocks = parse("Run:\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n\nDone.");
        assert_eq!(kinds(&blocks).len(), 3);
        assert_eq!(blocks[1].kind, BlockKind::Code("rust".into()));
        assert_eq!(blocks[1].text, "fn main() {\n    println!(\"hi\");\n}");
        let odd = parse("```a<b>\"; rm -rf /\ncode\n```");
        assert_eq!(odd[0].kind, BlockKind::Code("ab".into()));
    }

    #[test]
    fn links_are_ranges_and_only_web_links_survive() {
        let blocks = parse("See [the docs](https://example.org/x) and [local](file:///etc/passwd) or [js](javascript:alert(1)).");
        let p = &blocks[0];
        assert_eq!(p.text, "See the docs and local or js.");
        assert_eq!(p.links.len(), 1);
        assert_eq!(&p.text[p.links[0].start..p.links[0].end], "the docs");
        assert_eq!(p.links[0].url, "https://example.org/x");
        assert!(!is_web("https://a b"));
        assert!(!is_web("ftp://x"));
        assert!(is_web("HTTP://X.org"));
        // Bidi controls, look-alike letters and over-long addresses.
        assert!(!is_web("https://example.org/\u{202e}gpj.exe"));
        assert!(!is_web("https://\u{430}pple.com/"));
        assert!(!is_web(&format!("https://x.org/{}", "a".repeat(1000))));
    }

    #[test]
    fn raw_html_is_text_and_images_are_their_alt_text() {
        let blocks = parse("<img src=x onerror=alert(1)>\n\n![alt text](https://evil.test/p.png?d=secret)\n\n<script>bad()</script>\n\nA `Vec<String>` and <b>bold</b>.");
        // Nothing is interpreted: no link, no picture.
        assert!(blocks.iter().all(|b| b.links.is_empty()));
        let all: String = blocks
            .iter()
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("|");
        assert!(!all.contains("evil.test"), "{all}");
        assert!(all.contains("alt text"));
        // The markup is shown as written.
        assert!(all.contains("<script>bad()</script>"), "{all}");
        assert!(all.contains("<b>bold</b>"), "{all}");
        assert!(all.contains("Vec<String>"), "{all}");
    }

    #[test]
    fn html_in_a_list_item_keeps_its_bullet() {
        let blocks = parse(
            "- <b>x</b>

- y
",
        );
        assert!(
            blocks
                .iter()
                .all(|b| matches!(b.kind, BlockKind::Item { .. })),
            "{blocks:?}"
        );
        assert!(blocks[0].text.contains("<b>x</b>"));
    }

    #[test]
    fn a_huge_message_is_capped_not_slow() {
        let source = "line\n\n".repeat(MAX_BLOCKS * 2);
        let blocks = parse(&source);
        assert_eq!(blocks.len(), MAX_BLOCKS + 1);
        assert!(blocks.last().unwrap().text.contains("too many blocks"));
        // Pathological nesting and odd input never panic.
        for text in [
            "> > > > > > > x",
            "- - - - - x",
            "```",
            "\u{0}\u{1}",
            "[a](",
            "***",
            "# ",
        ] {
            let _ = parse(text);
        }
    }
}
