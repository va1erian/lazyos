//! Boot-log lines as data.
//!
//! The kernel and the drivers print `TAG:SUB key=value key="a b" words`
//! lines, optionally behind the serial port's `[12.345]` stamp. [`Line`]
//! splits one into the stamp, the tag, the `key=value` fields and the rest,
//! so a client filters on `tag == "USBD:DIAG"` instead of scraping text. A
//! line that does not look like that is still a line: `tag` is `None` and
//! `text` holds it whole.

use alloc::string::String;
use alloc::vec::Vec;

use crate::json::Object;

/// Most fields kept per line (the rest stay in `text`).
pub const MAX_FIELDS: usize = 24;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// Milliseconds from a `[secs.millis]` stamp.
    pub stamp_ms: Option<u64>,
    /// The first token when it is `UPPER:...` (a marker).
    pub tag: Option<String>,
    pub fields: Vec<(String, String)>,
    /// The line without the stamp.
    pub text: String,
}

fn stamp(line: &str) -> (Option<u64>, &str) {
    let Some(rest) = line.strip_prefix('[') else {
        return (None, line);
    };
    let Some((inside, after)) = rest.split_once(']') else {
        return (None, line);
    };
    let Some((secs, millis)) = inside.trim().split_once('.') else {
        return (None, line);
    };
    let digits = |s: &str| !s.is_empty() && s.len() <= 12 && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(secs) || !digits(millis) {
        return (None, line);
    }
    let value = secs.parse::<u64>().ok().and_then(|s| {
        let m = millis.parse::<u64>().ok()?;
        let scale = 10u64.pow(3u32.saturating_sub(millis.len() as u32));
        let divide = 10u64.pow((millis.len() as u32).saturating_sub(3));
        s.checked_mul(1000)?
            .checked_add(m.checked_mul(scale)? / divide)
    });
    (value, after.trim_start())
}

fn is_marker(token: &str) -> bool {
    let mut chars = token.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_uppercase())
        && token.contains(':')
        && !token.contains('=')
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '.' | '-' | '/'))
}

/// Split `line` (no newline).
pub fn parse(line: &str) -> Line {
    let (stamp_ms, text) = stamp(line);
    let mut tokens = text.split_whitespace().peekable();
    let tag = match tokens.peek() {
        Some(token) if is_marker(token) => tokens.next().map(String::from),
        _ => None,
    };
    let mut fields = Vec::new();
    if tag.is_some() {
        // Field values may be quoted and hold spaces: re-scan the text after
        // the tag instead of using the whitespace tokens.
        let after = text
            .trim_start()
            .strip_prefix(tag.as_deref().unwrap_or(""))
            .unwrap_or("");
        let bytes = after.as_bytes();
        let mut at = 0;
        while at < bytes.len() && fields.len() < MAX_FIELDS {
            while at < bytes.len() && bytes[at].is_ascii_whitespace() {
                at += 1;
            }
            let start = at;
            while at < bytes.len() && !bytes[at].is_ascii_whitespace() && bytes[at] != b'=' {
                at += 1;
            }
            if at >= bytes.len() || bytes[at] != b'=' {
                // A bare word: not a field; skip the rest of the token.
                while at < bytes.len() && !bytes[at].is_ascii_whitespace() {
                    at += 1;
                }
                continue;
            }
            let key = &after[start..at];
            at += 1;
            let value = if bytes.get(at) == Some(&b'"') {
                let begin = at + 1;
                at = begin;
                while at < bytes.len() && bytes[at] != b'"' {
                    at += 1;
                }
                let value = &after[begin..at.min(bytes.len())];
                at = (at + 1).min(bytes.len());
                value
            } else {
                let begin = at;
                while at < bytes.len() && !bytes[at].is_ascii_whitespace() {
                    at += 1;
                }
                &after[begin..at]
            };
            if !key.is_empty() {
                fields.push((String::from(key), String::from(value)));
            }
        }
    }
    Line {
        stamp_ms,
        tag,
        fields,
        text: String::from(text),
    }
}

impl Line {
    /// The value of field `key`.
    pub fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// `{"pos":..?,"t_ms":..?,"tag":..?,"fields":{..},"text":".."}`; `pos` is
    /// the byte offset of the line in the boot log when the caller knows it.
    pub fn to_json(&self, pos: Option<u64>) -> String {
        let mut object = Object::new();
        if let Some(pos) = pos {
            object = object.uint("pos", pos);
        }
        if let Some(ms) = self.stamp_ms {
            object = object.uint("t_ms", ms);
        }
        if let Some(tag) = &self.tag {
            object = object.str("tag", tag);
        }
        if !self.fields.is_empty() {
            let mut fields = Object::new();
            for (index, (key, value)) in self.fields.iter().enumerate() {
                // A repeated key keeps its first value; JSON objects take one.
                if !self.fields[..index].iter().any(|(k, _)| k == key) {
                    fields = fields.str(key, value);
                }
            }
            object = object.raw("fields", &fields.finish());
        }
        object.str("text", &self.text).finish()
    }
}

/// The complete lines of `text`, and the trailing partial line (no newline
/// yet), which a follower holds back until the rest arrives.
pub fn split_complete(text: &str) -> (Vec<&str>, &str) {
    match text.rfind('\n') {
        Some(end) => (text[..end].split('\n').collect(), &text[end + 1..]),
        None => (Vec::new(), text),
    }
}
