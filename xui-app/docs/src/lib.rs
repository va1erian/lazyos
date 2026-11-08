//! The pure, host-testable half of the Docs app: reading a document (bounded) and parsing it, the
//! page for one that cannot be read, what a link means and the documents visited.
//!
//! The page is drawn by xui's read-only Markdown view (`xui-markdown`), which shows raw HTML in a
//! document as text: nothing in a document reaches a layout engine as markup, and nothing is loaded.

pub mod history;
pub mod links;

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::Arc;

use xui_markdown::Document;

/// The largest document the app shows; a bigger file is cut off (with a
/// notice) rather than handed whole to the parser and the layout.
pub const MAX_BYTES: usize = 1 << 20;

/// What a cut-off document ends with.
const TRUNCATED: &str = "\n\n*(document truncated)*\n";

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

/// `markdown` cut to [`MAX_BYTES`] on a character boundary, with a notice
/// when anything was cut.
pub fn page(markdown: &str) -> String {
    if markdown.len() <= MAX_BYTES {
        return markdown.to_owned();
    }
    let mut end = MAX_BYTES;
    while !markdown.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + TRUNCATED.len());
    out.push_str(&markdown[..end]);
    out.push_str(TRUNCATED);
    out
}

/// Reads the Markdown file at `path` (bounded, see [`read_bounded`]) and parses
/// it. Slow for a big file: the app calls it on a worker thread.
pub fn load_file(path: &Path) -> io::Result<Arc<Document>> {
    let text = page(&read_bounded(File::open(path)?)?);
    Ok(Arc::new(Document::parse(&text)))
}

/// A page saying `message` under an error heading, for a document that could
/// not be opened. The message is escaped, so a path with Markdown characters
/// shows as written.
pub fn error_page(message: &str) -> String {
    format!(
        "# Could not open the document\n\n{}\n",
        escape_markdown(message)
    )
}

/// `text` as literal Markdown: every ASCII punctuation character
/// backslash-escaped (CommonMark allows it before any of them) and line breaks
/// as spaces, so it stays one paragraph of plain text.
fn escape_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        match c {
            '\n' | '\r' => out.push(' '),
            c if c.is_ascii_punctuation() => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests;
