//! Help > Report an issue: what the form collects and the Markdown it turns into
//! (docs/lazywebplus-plan.md, section 7, "Fallback").
//!
//! This is the part with no window, host-tested. Nothing in it reads a page: the report holds what
//! the person typed, the version, the page's host (its address only when they ask, stripped of
//! user, query and fragment), the window's own `Webplus` trace and the engine events this run saw.
//! Every piece is stripped of control and bidirectional characters and capped, and the whole is
//! at most [`MAX_TOTAL`] bytes, so a report can neither carry a hostile title into someone's
//! issue tracker nor grow past what a Messenger report may be (`os.lazy.agent.report.v1`, 256 KiB,
//! which "Send to LazyAgent" will use when LazyAgent exists: the button is a placeholder).

pub mod form;
pub mod ring;

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::sanitize;

/// The most bytes a report has.
pub const MAX_TOTAL: usize = 256 * 1024;
/// The most characters of each of the three things the person types.
pub const MAX_FIELD: usize = 4_000;
/// The longest host and address kept, in characters.
const MAX_HOST: usize = 253;
const MAX_ADDRESS: usize = 1_024;
/// How many lines of the window's trace a report holds.
pub const TRACE_LINES: usize = 200;
/// What the report says when it had to stop.
const CUT: &str = "\n\n(report cut: it was too long)\n";

/// Which of the optional parts the person ticked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Include {
    /// Version, tab count and platform.
    pub build: bool,
    /// The page's host.
    pub host: bool,
    /// The page's address, without user, query or fragment.
    pub address: bool,
    pub trace: bool,
    pub events: bool,
}

impl Default for Include {
    /// Only the version and tab count are on: everything else is the person's choice.
    fn default() -> Include {
        Include {
            build: true,
            host: false,
            address: false,
            trace: false,
            events: false,
        }
    }
}

/// What the window knows that a report may carry.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub version: String,
    pub tabs: usize,
    pub platform: String,
    /// The address of the page on show, or empty when it is one of the browser's own.
    pub page_url: String,
    /// The newest lines of the window's trace, oldest first.
    pub trace: Vec<String>,
    /// The engine events this run saw, oldest first.
    pub events: Vec<String>,
}

/// What the person typed.
#[derive(Clone, Copy, Debug, Default)]
pub struct Typed<'a> {
    pub happened: &'a str,
    pub expected: &'a str,
    pub steps: &'a str,
}

/// The host of an `http:` or `https:` address, lower case, without user, port or path. Brackets
/// stay around an IPv6 address.
pub fn host_of(url: &str) -> Option<String> {
    let authority = authority_of(url)?;
    let host = match authority.strip_prefix('[') {
        Some(rest) => &authority[..rest.find(']')? + 2],
        None => authority.split(':').next().unwrap_or(""),
    };
    let host = sanitize::line(host, MAX_HOST).to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

/// The address of an `http:` or `https:` page as far as its path: no user and password, query
/// string or fragment.
pub fn address_of(url: &str) -> Option<String> {
    let authority = authority_of(url)?;
    let url = url.trim();
    let scheme_end = url.find("://")?;
    let scheme = url[..scheme_end].to_ascii_lowercase();
    let rest = &url[scheme_end + 3..];
    let path = rest
        .find(['/', '?', '#'])
        .map_or("", |at| &rest[at..])
        .split(['?', '#'])
        .next()
        .unwrap_or("");
    let address = format!("{scheme}://{}{path}", authority.to_ascii_lowercase());
    Some(sanitize::line(&address, MAX_ADDRESS))
}

/// The part between `://` and the first `/`, `?` or `#` of a web address, without `user:pass@`.
fn authority_of(url: &str) -> Option<&str> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit('@').next()?;
    (!authority.is_empty()).then_some(authority)
}

/// The Markdown report for what was typed, what was ticked and what the window knows.
pub fn markdown(typed: Typed<'_>, include: Include, facts: &Facts) -> String {
    let mut sections = vec![String::from("# LazyWeb+ issue report\n")];
    let field = |title: &str, text: &str| {
        let text = sanitize::clip_chars(&sanitize::strip_multiline(text), MAX_FIELD);
        let text = text.trim();
        let text = if text.is_empty() {
            "(nothing written)"
        } else {
            text
        };
        format!("## {title}\n\n{text}\n")
    };
    sections.push(field("What happened", typed.happened));
    sections.push(field("What I expected", typed.expected));
    sections.push(field("Steps to reproduce", typed.steps));
    if include.build {
        sections.push(format!(
            "## Build\n\n- LazyWeb+ {}\n- Tabs: {}\n- Platform: {}\n",
            sanitize::line(&facts.version, 64),
            facts.tabs,
            sanitize::line(&facts.platform, 64),
        ));
    }
    let host = include.host.then(|| host_of(&facts.page_url)).flatten();
    let address = include
        .address
        .then(|| address_of(&facts.page_url))
        .flatten();
    if include.host || include.address {
        let mut page = String::from("## Page\n\n");
        match (&host, &address) {
            (None, None) => page.push_str("- (no web page was on show)\n"),
            _ => {
                if let Some(host) = &host {
                    page.push_str(&format!("- Host: {host}\n"));
                }
                if let Some(address) = &address {
                    page.push_str(&format!(
                        "- Address (user, query and fragment removed): {address}\n"
                    ));
                }
            }
        }
        sections.push(page);
    }
    if include.trace {
        sections.push(trace_section(&facts.trace));
    }
    if include.events {
        sections.push(events_section(&facts.events));
    }
    assemble(&sections)
}

/// The sections joined, stopping before [`MAX_TOTAL`] bytes.
fn assemble(sections: &[String]) -> String {
    // Room for the newline that joins the last section and for the note.
    let limit = MAX_TOTAL - CUT.len() - 1;
    let mut out = String::new();
    for section in sections {
        let wanted = section.len() + usize::from(!out.is_empty());
        if out.len() + wanted > limit {
            let room = limit.saturating_sub(out.len() + 1);
            out.push('\n');
            out.push_str(sanitize::clip_bytes(section, room));
            out.push_str(CUT);
            return out;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(section);
    }
    out
}

fn trace_section(lines: &[String]) -> String {
    let skip = lines.len().saturating_sub(TRACE_LINES);
    let body: Vec<String> = lines[skip..]
        .iter()
        .map(|line| sanitize::line(line, ring::LINE_MAX))
        .collect();
    let body = body.join("\n");
    // A fence longer than any run of backticks in the lines, so no line can end it.
    let fence = "`".repeat(longest_run(&body, '`').max(2) + 1);
    format!(
        "## Window trace (last {} lines)\n\n{fence}text\n{body}\n{fence}\n",
        lines.len() - skip
    )
}

fn events_section(events: &[String]) -> String {
    if events.is_empty() {
        return String::from("## Engine events\n\n- none this run\n");
    }
    let mut out = String::from("## Engine events\n\n");
    for event in &events[events.len().saturating_sub(ring::EVENTS_MAX)..] {
        out.push_str(&format!("- {}\n", sanitize::line(event, 200)));
    }
    out
}

fn longest_run(text: &str, c: char) -> usize {
    let (mut best, mut run) = (0, 0);
    for ch in text.chars() {
        run = if ch == c { run + 1 } else { 0 };
        best = best.max(run);
    }
    best
}

/// One line of what each tick box would add, for the form to show beside it.
pub fn previews(facts: &Facts) -> [String; 5] {
    let one = |text: String| sanitize::clip_chars(&text, 90);
    [
        one(format!(
            "LazyWeb+ {}, {} tab, {}",
            facts.version, facts.tabs, facts.platform
        )),
        one(host_of(&facts.page_url).unwrap_or_else(|| String::from("(no web page on show)"))),
        one(address_of(&facts.page_url).unwrap_or_else(|| String::from("(no web page on show)"))),
        one(match facts.trace.last() {
            Some(last) => format!(
                "{} line{}, the newest: {last}",
                facts.trace.len(),
                if facts.trace.len() == 1 { "" } else { "s" }
            ),
            None => String::from("no lines yet"),
        }),
        one(match facts.events.len() {
            0 => String::from("none this run"),
            1 => format!("1 event: {}", facts.events[0]),
            n => format!("{n} events, the newest: {}", facts.events[n - 1]),
        }),
    ]
}

/// The file name for a report made at `now` (seconds since the epoch).
pub fn file_name(now: u64) -> String {
    format!("lazywebplus-report-{now}.md")
}

/// Writes `markdown` as a new file in the Downloads folder `dir` (made if missing), named with
/// `now`, `(1)` and so on when the name is taken. It never replaces a file.
pub fn save(dir: &Path, now: u64, markdown: &str) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    for _ in 0..100 {
        let path =
            lazydownloads::unique_path(dir, &lazydownloads::sanitize(&file_name(now)), |_| false);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                file.write_all(markdown.as_bytes())?;
                file.sync_all()?;
                return Ok(path);
            }
            // Somebody took the name between the look and the create: look again.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free file name",
    ))
}

#[cfg(test)]
mod tests;
