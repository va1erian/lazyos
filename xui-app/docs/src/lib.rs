//! Markdown to a styled HTML page: the pure, host-testable half of the Docs app.
//!
//! The page goes to litehtml, which has no scripting: the only untrusted-input
//! concern is markup smuggled through the Markdown, so raw HTML in the source
//! is rendered as literal text and never reaches the layout engine as markup.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use pulldown_cmark::{html, Event, Options, Parser};

/// The largest document the app renders; a bigger file is cut off (with a
/// notice) rather than handed whole to the parser and layout engine.
pub const MAX_BYTES: usize = 1 << 20;

/// Reads at most [`MAX_BYTES`] + 1 bytes of `reader` as text, so a huge file is
/// never held whole in memory: the extra byte lets [`page`] see that the
/// document was longer than the limit and add its truncation notice. Invalid
/// UTF-8, including a multi-byte character cut by the limit, becomes U+FFFD
/// instead of failing the open.
pub fn read_bounded(reader: impl Read) -> io::Result<String> {
    let mut bytes = Vec::new();
    reader.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The page stylesheet: Droid Sans body, JetBrains Mono code, light theme.
const CSS: &str = "\
body { font-family: 'Droid Sans'; font-size: 15px; line-height: 1.5; color: #1f2328;
       background: #ffffff; margin: 24px 32px; }
h1, h2, h3, h4 { font-weight: bold; margin: 18px 0 8px 0; }
h1 { font-size: 28px; border-bottom: 1px solid #d0d7de; padding-bottom: 6px; }
h2 { font-size: 22px; border-bottom: 1px solid #d0d7de; padding-bottom: 4px; }
h3 { font-size: 18px; } h4 { font-size: 15px; }
p { margin: 8px 0; }
a { color: #0969da; text-decoration: underline; }
code { font-family: 'JetBrains Mono'; font-size: 13px; background: #eff1f3; }
pre { font-family: 'JetBrains Mono'; font-size: 13px; background: #f6f8fa; border: 1px solid #d0d7de;
      padding: 10px 12px; margin: 10px 0; }
pre code { background: #f6f8fa; }
blockquote { border-left: 4px solid #d0d7de; color: #57606a; margin: 8px 0; padding: 0 14px; }
table { border-collapse: collapse; margin: 10px 0; }
th, td { border: 1px solid #d0d7de; padding: 5px 12px; }
th { background: #f6f8fa; font-weight: bold; }
ul, ol { margin: 8px 0; padding-left: 28px; }
li { margin: 3px 0; }
hr { border: 0; border-top: 1px solid #d0d7de; margin: 16px 0; }";

/// Renders `markdown` (truncated to [`MAX_BYTES`] on a character boundary) as a
/// complete HTML page.
pub fn page(markdown: &str) -> String {
    let (markdown, cut) = truncate(markdown);
    // No task lists: they render as `<input>` checkboxes, which litehtml does
    // not draw.
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    // Raw HTML becomes text: `push_html` escapes a `Text` event.
    let events = Parser::new_ext(markdown, options).map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        other => other,
    });
    let mut body = String::with_capacity(markdown.len() * 2);
    html::push_html(&mut body, events);
    if cut {
        body.push_str("<p><i>(document truncated)</i></p>");
    }
    wrap(&body)
}

/// `body` inside the viewer's document skeleton and stylesheet.
fn wrap(body: &str) -> String {
    format!("<!doctype html><meta charset=\"utf-8\"><style>{CSS}</style><body>{body}</body>")
}

/// Reads the Markdown file at `path` (bounded, see [`read_bounded`]) and renders
/// it as a page.
pub fn load_file(path: &Path) -> io::Result<String> {
    Ok(page(&read_bounded(File::open(path)?)?))
}

/// A page saying `message` under an error heading, for a document that could
/// not be opened. The message is escaped, so a path with markup characters
/// shows as text.
pub fn error_page(message: &str) -> String {
    wrap(&format!(
        "<h1>Could not open the document</h1><p>{}</p>",
        escape_html(message)
    ))
}

/// `text` with the characters HTML gives meaning to replaced by entities.
fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

/// `markdown` cut to at most [`MAX_BYTES`] on a `char` boundary, and whether
/// anything was cut.
fn truncate(markdown: &str) -> (&str, bool) {
    if markdown.len() <= MAX_BYTES {
        return (markdown, false);
    }
    let mut end = MAX_BYTES;
    while !markdown.is_char_boundary(end) {
        end -= 1;
    }
    (&markdown[..end], true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_headings_emphasis_and_tables() {
        let html = page("# Title\n\nsome **bold** text\n\n|a|b|\n|-|-|\n|1|2|\n");
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.contains("<table>"));
    }

    #[test]
    fn raw_html_is_escaped_not_passed_through() {
        let html = page("<script>alert(1)</script>\n\ninline <img src=x onerror=y> here");
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<img"));
        assert!(html.contains("&lt;script&gt;"));
    }

    #[test]
    fn oversized_input_is_truncated_on_a_char_boundary() {
        let big = "é".repeat(MAX_BYTES); // 2 bytes each, so the cut lands mid-run
        let html = page(&big);
        assert!(html.contains("(document truncated)"));
        assert!(html.len() < MAX_BYTES * 3);
    }

    #[test]
    fn empty_input_is_a_valid_page() {
        assert!(page("").contains("<body></body>"));
    }

    /// A reader that counts how much was pulled from it.
    struct Counting<'a>(&'a std::cell::Cell<usize>, usize);

    impl Read for Counting<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(self.1);
            buf[..n].fill(b'a');
            self.1 -= n;
            self.0.set(self.0.get() + n);
            Ok(n)
        }
    }

    #[test]
    fn a_huge_file_is_read_only_up_to_the_limit_plus_one() {
        let pulled = std::cell::Cell::new(0);
        let text = read_bounded(Counting(&pulled, 50 * MAX_BYTES)).expect("reads");
        assert_eq!(text.len(), MAX_BYTES + 1);
        assert_eq!(
            pulled.get(),
            MAX_BYTES + 1,
            "nothing past the limit was read"
        );
        assert!(page(&text).contains("(document truncated)"));
    }

    #[test]
    fn a_file_at_the_limit_is_not_reported_truncated() {
        let text = read_bounded(&vec![b'a'; MAX_BYTES][..]).expect("reads");
        assert!(!page(&text).contains("(document truncated)"));
    }

    #[test]
    fn a_multibyte_character_cut_by_the_limit_does_not_fail_the_read() {
        // 2-byte characters: the limit + 1 byte lands mid-character.
        let bytes = "\u{e9}".repeat(MAX_BYTES).into_bytes();
        let text = read_bounded(&bytes[..]).expect("reads");
        assert!(text.ends_with('\u{FFFD}'));
        assert!(page(&text).contains("(document truncated)"));
    }

    #[test]
    fn invalid_utf8_is_replaced_not_rejected() {
        let text = read_bounded(&b"ok \xFF\xFE done"[..]).expect("reads");
        assert_eq!(text, "ok \u{FFFD}\u{FFFD} done");
    }

    /// The fixture the image ships as `/system/share/samples/testdoc.md`, which
    /// the Docs session opens.
    const TESTDOC: &str = include_str!("../testdata/testdoc.md");

    #[test]
    fn the_test_document_renders_every_feature_it_advertises() {
        let html = page(TESTDOC);
        for wanted in [
            "<h1>",
            "<h2>",
            "<h3>",
            "<h4>",
            "<table>",
            "<blockquote>",
            "<pre>",
            "<code>",
            "<ul>",
            "<ol>",
            "<strong>",
            "<em>",
            "<del>",
            "<hr />",
            "<a href=",
        ] {
            assert!(html.contains(wanted), "the test document lacks {wanted}");
        }
        assert!(!html.contains("<script"), "raw HTML must stay escaped");
        assert!(!html.contains("(document truncated)"));
        assert!(html.len() > 4000, "long enough to scroll");
    }

    #[test]
    fn load_file_reads_and_renders_a_file() {
        let dir = std::env::temp_dir().join(format!("xui-docs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("note.md");
        std::fs::write(&path, "# From disk\n\ntext").unwrap();
        let html = load_file(&path).expect("loads");
        assert!(html.contains("<h1>From disk</h1>"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_file_reports_a_missing_file() {
        let error = load_file(Path::new("/definitely/not/here.md")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn the_error_page_escapes_the_message() {
        let html = error_page("no <b>such</b> & \"file\"");
        assert!(html.contains("Could not open the document"));
        assert!(html.contains("no &lt;b&gt;such&lt;/b&gt; &amp; &quot;file&quot;"));
        assert!(!html.contains("<b>such"));
    }
}
