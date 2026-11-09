//! A doc page as Markdown, for the doc pane's Markdown view, and what a click
//! on one of its links means.
//!
//! The schema's own words (the IDL doc comments, [`Block::Paragraph`]) are
//! Markdown already and go through as written, so their emphasis, code, lists
//! and links show as their authors meant. Everything the playground writes
//! itself (names, signatures, types, list items) is escaped or fenced, so a
//! `*` or `_` in a topic pattern or a type is shown, not taken as formatting.

use crate::docs::{Block, ChipKind, DocLink, FieldRow};

/// `blocks` as one Markdown document.
pub fn page(blocks: &[Block]) -> String {
    let mut out = String::new();
    for block in blocks {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        push_block(&mut out, block);
    }
    out.push('\n');
    out
}

fn push_block(out: &mut String, block: &Block) {
    match block {
        Block::Title(text) => {
            out.push_str("# ");
            out.push_str(&escape(text));
        }
        Block::Heading(text) => {
            out.push_str("## ");
            out.push_str(&escape(text));
        }
        Block::Paragraph(text) => out.push_str(text.trim()),
        Block::Signature(text) => push_fence(out, "", text),
        Block::Code { label, text } => {
            out.push_str(&format!("**{}**\n\n", escape(label)));
            push_fence(out, "rhai", text);
        }
        Block::Fields(rows) => push_fields(out, rows),
        Block::List(items) => push_list(out, items.iter().map(|item| escape(item))),
        Block::Chips(chips) => {
            for (n, chip) in chips.iter().enumerate() {
                if n > 0 {
                    out.push_str("\n>\n");
                }
                out.push_str("> ");
                if chip.kind == ChipKind::Warning {
                    out.push_str("**Warning:** ");
                }
                out.push_str(&escape(&chip.text));
            }
        }
        Block::Links(links) => push_list(out, links.iter().map(link)),
    }
}

/// `text` in a fenced code block tagged `lang`, the fence longer than any run
/// of backticks in it.
fn push_fence(out: &mut String, lang: &str, text: &str) {
    let fence = "`".repeat(longest_run(text, '`').max(2) + 1);
    out.push_str(&format!(
        "{fence}{lang}\n{}\n{fence}",
        text.trim_end_matches('\n')
    ));
}

fn push_list(out: &mut String, items: impl Iterator<Item = String>) {
    let lines: Vec<String> = items.map(|item| format!("- {item}")).collect();
    out.push_str(&lines.join("\n"));
}

/// A field table: name and type as code, the note as text.
fn push_fields(out: &mut String, rows: &[FieldRow]) {
    let notes = rows.iter().any(|row| !row.note.is_empty());
    out.push_str(if notes {
        "| Field | Type | Note |\n|---|---|---|"
    } else {
        "| Field | Type |\n|---|---|"
    });
    for row in rows {
        out.push_str(&format!(
            "\n| {} | {} |",
            cell_code(&row.name),
            cell_code(&row.ty)
        ));
        if notes {
            out.push_str(&format!(" {} |", escape(&row.note)));
        }
    }
}

fn link(link: &DocLink) -> String {
    // `<...>` keeps a path with spaces or parentheses whole.
    format!(
        "[{}](<{}>)",
        escape(&link.label),
        link.path.replace(['<', '>'], "")
    )
}

/// `text` as a code span inside a table cell (a `|` would end the cell).
fn cell_code(text: &str) -> String {
    if text.is_empty() {
        return String::new();
    }
    let ticks = "`".repeat(longest_run(text, '`') + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{}{pad}{ticks}", text.replace('|', "\\|"))
}

/// `text` as literal Markdown: ASCII punctuation backslash-escaped
/// (CommonMark allows it before any of them) and line breaks as spaces.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
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

fn longest_run(text: &str, of: char) -> usize {
    let (mut best, mut run) = (0, 0);
    for c in text.chars() {
        run = if c == of { run + 1 } else { 0 };
        best = best.max(run);
    }
    best
}

/// What a click on a link in a doc page does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// A web or mail address, for the app registered for its scheme.
    Url(String),
    /// A file named by an absolute path (a Markdown page goes to Docs), for `mimed`.
    File(String),
    /// Nothing the playground follows: a relative path (a doc comment has no
    /// folder to resolve it in), another scheme, or nonsense.
    Refused,
}

/// Longest link the playground looks at (a doc comment can write any length).
const MAX_LINK: usize = 2048;

/// Classifies `href` as the page reported it (a link to one of the page's own
/// headings never reaches here: the view scrolls to it).
pub fn classify(href: &str) -> Link {
    let href = href.trim();
    if href.is_empty() || href.len() > MAX_LINK || href.chars().any(|c| c.is_control()) {
        return Link::Refused;
    }
    let lower = href.to_ascii_lowercase();
    if ["http://", "https://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
    {
        return Link::Url(href.to_owned());
    }
    // A launch line is split at spaces, and `..` would let a doc comment
    // name any file: an absolute path, written plainly, only.
    // The path as a file name: no query or fragment, escapes decoded.
    let Some(path) = percent_decode(href.split(['#', '?']).next().unwrap_or("")) else {
        return Link::Refused;
    };
    let plain = path.starts_with('/')
        && !path.contains(' ')
        && !path.chars().any(|c| c.is_control())
        && !path.split('/').any(|part| part == "..");
    if plain {
        Link::File(path)
    } else {
        Link::Refused
    }
}

/// `%20` and the like decoded; `None` when an escape is cut short or the bytes are not UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = std::str::from_utf8(bytes.get(at + 1..at + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}
