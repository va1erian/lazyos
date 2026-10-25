//! What was open, kept so a crash does not lose it: `session.json` in the profile folder, and a
//! `running` marker that says the last run did not end cleanly when it is still there at start.
//!
//! The format is the multi-tab browser's (`{"version":1,"windows":[{"tabs":[{"url":...,"title":...}],
//! "active":0,"closed":[...]}]}`, `closed` being the tabs closed last, newest first; a file without
//! it is as valid). `tabs.rs` makes and restores the windows. The file is written through a
//! temporary file and renamed, at most once a second ([`Throttle`]), only for pages from the web,
//! and never when the page's data is kept in memory (a person who asked for that does not want the
//! pages they browse written down; `memory.rs`). Both files are the profile folder's, which the
//! engine's processes may write, so what is read from them is untrusted: bounded in size, depth and
//! length, and only `http(s)` addresses are taken.

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use crate::address;
use crate::visits::{MAX_TITLE, MAX_URL};

/// The marker present while a window runs.
pub const RUNNING: &str = "running";
/// The saved session.
pub const FILE: &str = "session.json";
/// At most this much of the file is read, and at most this many windows and tabs are kept.
const MAX_FILE: u64 = 256 * 1024;
const MAX_WINDOWS: usize = 8;
const MAX_TABS: usize = 64;
/// The closed tabs a window remembers.
pub const MAX_CLOSED: usize = 10;
/// The shortest time between two writes.
const WRITE_EVERY_MS: u64 = 1000;
/// How deep a value may nest (the format needs four levels).
const MAX_DEPTH: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub url: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    pub tabs: Vec<Tab>,
    pub active: usize,
    /// The tabs closed last, newest first (Ctrl+Shift+T brings them back).
    pub closed: Vec<Tab>,
    /// Where each closed tab was among `tabs`, parallel to `closed`; `usize::MAX`, or no entry (a
    /// file from before positions were kept), brings it back at the end.
    pub closed_at: Vec<usize>,
}

impl Window {
    /// A window with these tabs and no closed ones.
    pub fn new(tabs: Vec<Tab>, active: usize) -> Window {
        Window {
            tabs,
            active,
            closed: Vec::new(),
            closed_at: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub windows: Vec<Window>,
}

impl Session {
    /// A session of one window with one tab.
    pub fn single(url: &str, title: &str) -> Session {
        Session {
            windows: vec![Window::new(
                vec![Tab {
                    url: url.to_owned(),
                    title: title.to_owned(),
                }],
                0,
            )],
        }
    }

    /// The tab to bring back: the first window's active one.
    pub fn page(&self) -> Option<&Tab> {
        let window = self.windows.first()?;
        window.tabs.get(window.active)
    }

    /// The file's text.
    pub fn to_json(&self) -> String {
        let windows: Vec<String> = self
            .windows
            .iter()
            .map(|w| {
                format!(
                    "{{\"tabs\":[{}],\"active\":{},\"closed\":[{}]}}",
                    tabs_json(&w.tabs, &[]),
                    w.active,
                    tabs_json(&w.closed, &w.closed_at)
                )
            })
            .collect();
        format!("{{\"version\":1,\"windows\":[{}]}}\n", windows.join(","))
    }

    /// The session in `text`, or `None` when it is not one this version understands. Tabs that are
    /// not web pages are dropped; the rest is kept as far as it is valid.
    pub fn parse(text: &str) -> Option<Session> {
        let value = json::parse(text)?;
        if value.get("version")?.as_number()? != 1.0 {
            return None;
        }
        let mut windows = Vec::new();
        for window in value.get("windows")?.as_array()?.iter().take(MAX_WINDOWS) {
            let active = window
                .get("active")
                .and_then(json::Value::as_number)
                .filter(|n| *n >= 0.0 && n.fract() == 0.0)
                .map_or(0, |n| n as usize);
            let (tabs, active) = parse_tabs(window.get("tabs")?.as_array()?, MAX_TABS, active);
            let (closed, closed_at) = window
                .get("closed")
                .and_then(json::Value::as_array)
                .map_or_else(Default::default, parse_closed);
            if !tabs.is_empty() {
                let active = active.min(tabs.len() - 1);
                windows.push(Window {
                    tabs,
                    active,
                    closed,
                    closed_at,
                });
            }
        }
        Some(Session { windows })
    }
}

/// The tab `value` describes, when it is a web page.
fn web_tab(value: &json::Value) -> Option<Tab> {
    let url = value.get("url").and_then(json::Value::as_str).filter(|u| {
        u.len() <= MAX_URL && address::is_network(u) && !u.chars().any(char::is_control)
    })?;
    let title = value
        .get("title")
        .and_then(json::Value::as_str)
        .unwrap_or("");
    Some(Tab {
        url: url.to_owned(),
        title: clean_title(title),
    })
}

/// The tabs of `list` that are web pages (of its first `limit`), and the number of the one that was
/// at `active` once the dropped ones before it are not counted.
fn parse_tabs(list: &[json::Value], limit: usize, mut active: usize) -> (Vec<Tab>, usize) {
    let mut tabs = Vec::new();
    for (at, tab) in list.iter().take(limit).enumerate() {
        match web_tab(tab) {
            Some(tab) => tabs.push(tab),
            None if at < active => active = active.saturating_sub(1),
            None => {}
        }
    }
    (tabs, active)
}

/// The closed tabs of `list` that are web pages and where each was (`at`, when the file says).
fn parse_closed(list: &[json::Value]) -> (Vec<Tab>, Vec<usize>) {
    let mut tabs = Vec::new();
    let mut places = Vec::new();
    for value in list.iter().take(MAX_CLOSED) {
        let Some(tab) = web_tab(value) else { continue };
        let at = value
            .get("at")
            .and_then(json::Value::as_number)
            .filter(|n| *n >= 0.0 && n.fract() == 0.0 && *n <= MAX_TABS as f64)
            .map_or(usize::MAX, |n| n as usize);
        tabs.push(tab);
        places.push(at);
    }
    (tabs, places)
}

/// `tabs` as the comma-separated objects of a JSON array; `places` (parallel, `usize::MAX` for
/// none) adds where each was.
fn tabs_json(tabs: &[Tab], places: &[usize]) -> String {
    let items: Vec<String> = tabs
        .iter()
        .enumerate()
        .map(|(n, t)| {
            let at = places
                .get(n)
                .filter(|at| **at <= MAX_TABS)
                .map_or_else(String::new, |at| format!(",\"at\":{at}"));
            format!(
                "{{\"url\":{},\"title\":{}{at}}}",
                quote(&t.url),
                quote(&t.title)
            )
        })
        .collect();
    items.join(",")
}

/// `title` on one line and no longer than the history keeps one.
pub fn clean_title(title: &str) -> String {
    title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_TITLE)
        .collect()
}

/// `text` as a JSON string.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether the last run crashed, and the marker for this one. Call once at start with the profile
/// folder: a marker left behind is a crash (`session/marker.rs` says why no liveness check).
pub fn begin(profile: &Path) -> bool {
    let path = profile.join(RUNNING);
    let crashed = path.exists();
    // A failure to write the marker only means a crash goes unnoticed.
    let _ = fs::write(&path, marker::text(std::process::id()));
    crashed
}

/// The window closed on purpose: the next start is not a crash. A marker a later process wrote
/// is left in place.
pub fn end(profile: &Path) {
    let path = profile.join(RUNNING);
    if let Ok(existing) = fs::read_to_string(&path) {
        if marker::is_own(&existing, std::process::id()) {
            let _ = fs::remove_file(&path);
        }
    }
}

/// The saved session, if there is one that parses.
pub fn load(profile: &Path) -> Option<Session> {
    let mut bytes = Vec::new();
    fs::File::open(profile.join(FILE))
        .ok()?
        .take(MAX_FILE)
        .read_to_end(&mut bytes)
        .ok()?;
    Session::parse(&String::from_utf8_lossy(&bytes))
}

/// Writes `session` through a temporary file, so a crash while writing leaves the old one.
pub fn save(profile: &Path, session: &Session) -> io::Result<()> {
    let tmp = profile.join("session.json.tmp");
    // Synced before the rename: otherwise a power cut can leave the new name on empty data.
    let mut file = fs::File::create(&tmp)?;
    file.write_all(session.to_json().as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, profile.join(FILE))?;
    // So that the rename itself survives; best effort (a directory cannot be opened everywhere).
    #[cfg(unix)]
    if let Ok(dir) = fs::File::open(profile) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Decides when a state is written: at once the first time, then at most once a second, the
/// newest state last. Pure, so the clock is the caller's. `T` is what is written: the page on
/// show (a [`Tab`]) or the whole [`Session`].
#[derive(Debug)]
pub struct Throttle<T = Tab> {
    last: Option<u64>,
    written: Option<T>,
    waiting: Option<T>,
}

impl<T> Default for Throttle<T> {
    fn default() -> Self {
        Throttle {
            last: None,
            written: None,
            waiting: None,
        }
    }
}

impl<T: Clone + PartialEq> Throttle<T> {
    /// `state` is what is on show at `now_ms`. Returns what to write now, if anything.
    pub fn note(&mut self, state: T, now_ms: u64) -> Option<T> {
        if self.written.as_ref() == Some(&state) {
            self.waiting = None;
            return None;
        }
        self.waiting = Some(state);
        self.due(now_ms)
    }

    /// Whether a state waits for its second to pass: returns it once it has.
    pub fn due(&mut self, now_ms: u64) -> Option<T> {
        let ready = self
            .last
            .is_none_or(|last| now_ms.saturating_sub(last) >= WRITE_EVERY_MS);
        if !ready {
            return None;
        }
        let state = self.waiting.take()?;
        self.last = Some(now_ms);
        self.written = Some(state.clone());
        Some(state)
    }
}

mod json;

pub mod marker;

#[cfg(test)]
mod tests;
