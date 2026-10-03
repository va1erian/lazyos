//! The package permissions a script needs, read from its source.
//!
//! An installed app runs under kernel rules compiled from its manifest
//! (`docs/packages.md`, "What a manifest compiles to"), so a LazyRAD app that
//! calls `confd` must declare `os.lazy.confd.v1`. [`derive`] finds what the
//! scripts use, from the same generated table the `sys::*` modules come from
//! (`super::api`):
//!
//! * `sys::<alias>::<function>(...)` needs the module's interface; a topic
//!   helper (`on_<t>`, `subscribe_<t>`, `publish_<t>`) needs
//!   `subscribe:`/`publish:` and the topic's declared filter instead;
//! * a string literal passed to `msg::connect`, `msg::on`, `msg::subscribe` or
//!   `msg::publish` adds that interface or topic.
//!
//! A name a script builds at run time cannot be seen; the consent screen shows
//! exactly what was derived. Entries that would break the manifest grammar
//! are left out rather than guessed at.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use super::api::{self, ApiModule};
use super::schema;

/// Interfaces and topic rules, sorted, in the manifest's grammar.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Derived {
    /// `os.lazy.confd.v1`, ...
    pub interfaces: Vec<String>,
    /// `subscribe:system/confd/changed/#`, `publish:...`
    pub topics: Vec<String>,
}

/// What `scripts` use.
pub fn derive<'a>(scripts: impl IntoIterator<Item = &'a str>) -> Derived {
    let mut interfaces = BTreeSet::new();
    let mut topics = BTreeSet::new();
    for script in scripts {
        let tokens = tokens(script);
        for (index, token) in tokens.iter().enumerate() {
            let Token::Path(path) = token else { continue };
            let literal = match (tokens.get(index + 1), tokens.get(index + 2)) {
                (Some(Token::Open), Some(Token::Str(text))) => Some(text.as_str()),
                _ => None,
            };
            match path.split("::").collect::<Vec<_>>().as_slice() {
                [api::NAMESPACE, alias, function] => {
                    if let Some(module) = api::module(alias) {
                        generated(module, function, &mut interfaces, &mut topics);
                    }
                }
                ["msg", "connect"] => {
                    if let Some(name) = literal.filter(|n| schema::interface(n).is_some()) {
                        interfaces.insert(name.to_string());
                    }
                }
                ["msg", "on" | "subscribe"] => add_topic(&mut topics, "subscribe", literal),
                ["msg", "publish"] => add_topic(&mut topics, "publish", literal),
                _ => {}
            }
        }
    }
    Derived {
        interfaces: interfaces.into_iter().collect(),
        topics: topics.into_iter().collect(),
    }
}

/// `sys::<module>::<function>`: a topic helper's rule, or the interface.
fn generated(
    module: &ApiModule,
    function: &str,
    interfaces: &mut BTreeSet<String>,
    topics: &mut BTreeSet<String>,
) {
    for topic in module.topics {
        let rule = match function {
            f if f == format!("on_{}", topic.helper) || f == format!("subscribe_{}", topic.helper) => {
                "subscribe"
            }
            f if f == format!("publish_{}", topic.helper) => "publish",
            // Names and patterns are plain strings: nothing is called.
            f if f == format!("{}_topic", topic.helper) => return,
            _ => continue,
        };
        topics.insert(format!("{rule}:{}", topic.pattern));
        return;
    }
    let constant = function.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
    if !constant && !function.starts_with("new_") {
        interfaces.insert(module.interface.to_string());
    }
}

fn add_topic(topics: &mut BTreeSet<String>, direction: &str, filter: Option<&str>) {
    if let Some(filter) = filter.filter(|f| valid_filter(f)) {
        topics.insert(format!("{direction}:{filter}"));
    }
}

/// The manifest's topic grammar: segments of `[a-z0-9_.-]+`, `+`, or a final
/// `#`.
fn valid_filter(filter: &str) -> bool {
    let segments: Vec<&str> = filter.split('/').collect();
    segments.iter().enumerate().all(|(index, segment)| match *segment {
        "+" => true,
        "#" => index + 1 == segments.len(),
        literal => {
            !literal.is_empty()
                && literal.bytes().all(|b| {
                    b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-')
                })
        }
    })
}

/// The few token kinds [`derive`] looks at.
#[derive(Debug, PartialEq, Eq)]
enum Token {
    /// An identifier or a `::` path (`sys::confd::get`).
    Path(String),
    /// A string literal's text (`"..."`, or a backtick string without `${}`).
    Str(String),
    Open,
    Other,
}

/// Split `source` into tokens, skipping comments and whitespace. Escapes in a
/// string end up verbatim, which only matters for names no service has.
fn tokens(source: &str) -> Vec<Token> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b.is_ascii_whitespace() {
            i += 1;
        } else if source[i..].starts_with("//") {
            i += source[i..].find('\n').unwrap_or(source.len() - i);
        } else if source[i..].starts_with("/*") {
            i += source[i + 2..].find("*/").map_or(source.len() - i, |end| end + 4);
        } else if b == b'"' || b == b'`' {
            let (text, next) = string(source, i);
            out.push(text.map_or(Token::Other, Token::Str));
            i = next;
        } else if b.is_ascii_alphabetic() || b == b'_' {
            let start = i;
            while i < bytes.len() {
                if bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' {
                    i += 1;
                } else if source[i..].starts_with("::") {
                    i += 2;
                } else {
                    break;
                }
            }
            out.push(Token::Path(source[start..i].to_string()));
        } else {
            out.push(if b == b'(' { Token::Open } else { Token::Other });
            i += source[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out
}

/// The string literal starting at `start` and the index after it. A
/// backtick string with an interpolation has no fixed text (`None`).
fn string(source: &str, start: usize) -> (Option<String>, usize) {
    let quote = source.as_bytes()[start];
    let mut text = String::new();
    let mut chars = source[start + 1..].char_indices();
    while let Some((offset, ch)) = chars.next() {
        match ch {
            '\\' => {
                if let Some((_, escaped)) = chars.next() {
                    text.push(escaped);
                }
            }
            c if c as u32 == u32::from(quote) => {
                let fixed = !(quote == b'`' && text.contains("${"));
                return (fixed.then_some(text), start + 1 + offset + 1);
            }
            c => text.push(c),
        }
    }
    (None, source.len())
}
