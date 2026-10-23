//! What was open, kept so a crash does not lose it: `session.json` in the profile folder, and a
//! `running` marker that says the last run did not end cleanly when it is still there at start.
//!
//! The format is the one the multi-tab browser will keep (`{"version":1,"windows":[{"tabs":[{"url":
//! ...,"title":...}],"active":0}]}`); this window has one window and one tab. The file is written
//! through a temporary file and renamed, at most once a second ([`Throttle`]), only for pages from
//! the web, and never when the page's data is kept in memory (a person who asked for that does not
//! want the pages they browse written down; `memory.rs`). Both files are the profile folder's, which
//! the engine's processes may write, so what is read from them is untrusted: bounded in size, depth
//! and length, and only `http(s)` addresses are taken.

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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub windows: Vec<Window>,
}

impl Session {
    /// A session of one window with one tab.
    pub fn single(url: &str, title: &str) -> Session {
        Session {
            windows: vec![Window {
                tabs: vec![Tab {
                    url: url.to_owned(),
                    title: title.to_owned(),
                }],
                active: 0,
            }],
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
                let tabs: Vec<String> = w
                    .tabs
                    .iter()
                    .map(|t| {
                        format!(
                            "{{\"url\":{},\"title\":{}}}",
                            quote(&t.url),
                            quote(&t.title)
                        )
                    })
                    .collect();
                format!("{{\"tabs\":[{}],\"active\":{}}}", tabs.join(","), w.active)
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
            let mut tabs = Vec::new();
            let mut active = window
                .get("active")
                .and_then(json::Value::as_number)
                .filter(|n| *n >= 0.0 && n.fract() == 0.0)
                .map_or(0, |n| n as usize);
            let listed = window.get("tabs")?.as_array()?;
            for (at, tab) in listed.iter().take(MAX_TABS).enumerate() {
                let Some(url) = tab.get("url").and_then(json::Value::as_str) else {
                    continue;
                };
                if url.len() > MAX_URL
                    || !address::is_network(url)
                    || url.chars().any(char::is_control)
                {
                    // The active tab's number counts the tabs before it that were dropped.
                    if at < active {
                        active = active.saturating_sub(1);
                    }
                    continue;
                }
                let title = tab.get("title").and_then(json::Value::as_str).unwrap_or("");
                tabs.push(Tab {
                    url: url.to_owned(),
                    title: clean_title(title),
                });
            }
            if !tabs.is_empty() {
                let active = active.min(tabs.len() - 1);
                windows.push(Window { tabs, active });
            }
        }
        Some(Session { windows })
    }
}

/// `title` on one line and no longer than the history keeps one.
fn clean_title(title: &str) -> String {
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

/// Decides when a navigation is written: at once the first time, then at most once a second, the
/// newest page last. Pure, so the clock is the caller's.
#[derive(Debug, Default)]
pub struct Throttle {
    last: Option<u64>,
    written: Option<Tab>,
    waiting: Option<Tab>,
}

impl Throttle {
    /// The page `tab` is on show at `now_ms`. Returns what to write now, if anything.
    pub fn note(&mut self, tab: Tab, now_ms: u64) -> Option<Tab> {
        if self.written.as_ref() == Some(&tab) {
            self.waiting = None;
            return None;
        }
        self.waiting = Some(tab);
        self.due(now_ms)
    }

    /// Whether a page waits for its second to pass: returns it once it has.
    pub fn due(&mut self, now_ms: u64) -> Option<Tab> {
        let ready = self
            .last
            .is_none_or(|last| now_ms.saturating_sub(last) >= WRITE_EVERY_MS);
        if !ready {
            return None;
        }
        let tab = self.waiting.take()?;
        self.last = Some(now_ms);
        self.written = Some(tab.clone());
        Some(tab)
    }
}

/// A small, strict JSON reader: only what a hostile file could use to waste time or memory is
/// bounded (depth and size are capped by the caller and [`MAX_DEPTH`]).
mod json {
    use super::MAX_DEPTH;

    #[derive(Debug)]
    pub enum Value {
        Null,
        /// A boolean: accepted, never read.
        Bool,
        Number(f64),
        Str(String),
        Array(Vec<Value>),
        Object(Vec<(String, Value)>),
    }

    impl Value {
        pub fn get(&self, key: &str) -> Option<&Value> {
            match self {
                Value::Object(fields) => fields.iter().find(|(k, _)| k == key).map(|(_, v)| v),
                _ => None,
            }
        }

        pub fn as_array(&self) -> Option<&[Value]> {
            match self {
                Value::Array(items) => Some(items),
                _ => None,
            }
        }

        pub fn as_str(&self) -> Option<&str> {
            match self {
                Value::Str(text) => Some(text),
                _ => None,
            }
        }

        pub fn as_number(&self) -> Option<f64> {
            match self {
                Value::Number(n) => Some(*n),
                _ => None,
            }
        }
    }

    /// The one value in `text` (and nothing after it but blanks).
    pub fn parse(text: &str) -> Option<Value> {
        let mut parser = Parser {
            bytes: text.as_bytes(),
            at: 0,
        };
        let value = parser.value(0)?;
        parser.blanks();
        (parser.at == parser.bytes.len()).then_some(value)
    }

    struct Parser<'a> {
        bytes: &'a [u8],
        at: usize,
    }

    impl Parser<'_> {
        fn blanks(&mut self) {
            while self
                .bytes
                .get(self.at)
                .is_some_and(|b| b" \t\r\n".contains(b))
            {
                self.at += 1;
            }
        }

        fn eat(&mut self, byte: u8) -> bool {
            self.blanks();
            let found = self.bytes.get(self.at) == Some(&byte);
            if found {
                self.at += 1;
            }
            found
        }

        fn word(&mut self, word: &str) -> bool {
            let found = self.bytes[self.at..].starts_with(word.as_bytes());
            if found {
                self.at += word.len();
            }
            found
        }

        fn value(&mut self, depth: usize) -> Option<Value> {
            if depth > MAX_DEPTH {
                return None;
            }
            self.blanks();
            match *self.bytes.get(self.at)? {
                b'{' => self.object(depth),
                b'[' => self.array(depth),
                b'"' => self.string().map(Value::Str),
                b't' => self.word("true").then_some(Value::Bool),
                b'f' => self.word("false").then_some(Value::Bool),
                b'n' => self.word("null").then_some(Value::Null),
                _ => self.number(),
            }
        }

        fn object(&mut self, depth: usize) -> Option<Value> {
            self.at += 1;
            let mut fields = Vec::new();
            if self.eat(b'}') {
                return Some(Value::Object(fields));
            }
            loop {
                self.blanks();
                let key = self.string()?;
                if !self.eat(b':') {
                    return None;
                }
                fields.push((key, self.value(depth + 1)?));
                if self.eat(b'}') {
                    return Some(Value::Object(fields));
                }
                if !self.eat(b',') {
                    return None;
                }
            }
        }

        fn array(&mut self, depth: usize) -> Option<Value> {
            self.at += 1;
            let mut items = Vec::new();
            if self.eat(b']') {
                return Some(Value::Array(items));
            }
            loop {
                items.push(self.value(depth + 1)?);
                if self.eat(b']') {
                    return Some(Value::Array(items));
                }
                if !self.eat(b',') {
                    return None;
                }
            }
        }

        fn number(&mut self) -> Option<Value> {
            let start = self.at;
            while self
                .bytes
                .get(self.at)
                .is_some_and(|b| b.is_ascii_digit() || b"+-.eE".contains(b))
            {
                self.at += 1;
            }
            let text = std::str::from_utf8(&self.bytes[start..self.at]).ok()?;
            text.parse()
                .ok()
                .filter(|n: &f64| n.is_finite())
                .map(Value::Number)
        }

        fn string(&mut self) -> Option<String> {
            if self.bytes.get(self.at) != Some(&b'"') {
                return None;
            }
            self.at += 1;
            let mut out: Vec<u8> = Vec::new();
            loop {
                let byte = *self.bytes.get(self.at)?;
                self.at += 1;
                match byte {
                    b'"' => return String::from_utf8(out).ok(),
                    b'\\' => {
                        let escape = *self.bytes.get(self.at)?;
                        self.at += 1;
                        let c = match escape {
                            b'"' => '"',
                            b'\\' => '\\',
                            b'/' => '/',
                            b'n' => '\n',
                            b'r' => '\r',
                            b't' => '\t',
                            b'b' => '\u{8}',
                            b'f' => '\u{c}',
                            b'u' => self.unicode()?,
                            _ => return None,
                        };
                        let mut buf = [0u8; 4];
                        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    }
                    // A raw control character is not valid JSON.
                    b if b < 0x20 => return None,
                    b => out.push(b),
                }
            }
        }

        /// `\uXXXX`, with a surrogate pair read as one character; a lone surrogate is refused.
        fn unicode(&mut self) -> Option<char> {
            let first = self.hex4()?;
            if (0xd800..0xdc00).contains(&first) {
                if !self.word("\\u") {
                    return None;
                }
                let second = self.hex4()?;
                if !(0xdc00..0xe000).contains(&second) {
                    return None;
                }
                return char::from_u32(0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00));
            }
            char::from_u32(first)
        }

        fn hex4(&mut self) -> Option<u32> {
            let digits = self.bytes.get(self.at..self.at + 4)?;
            let text = std::str::from_utf8(digits).ok()?;
            self.at += 4;
            u32::from_str_radix(text, 16).ok()
        }
    }
}

pub mod marker;

#[cfg(test)]
mod tests;
