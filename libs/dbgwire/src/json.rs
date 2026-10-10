//! A strict, small JSON reader and an escaping writer.
//!
//! `dbgd` reads requests from the network, so the reader is bounded on every
//! axis (nesting, string length, element count) and refuses anything it does
//! not understand rather than guessing. Numbers are kept as written when they
//! are integers (`Int`) and as `f64` otherwise.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

/// Deepest nesting of arrays and objects accepted.
pub const MAX_DEPTH: usize = 8;
/// Most elements in one array or object.
pub const MAX_ELEMENTS: usize = 64;
/// Longest string (bytes, after unescaping).
pub const MAX_STRING: usize = 8 * 1024;

/// A parsed JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    Array(Vec<Value>),
    /// Members in document order; a duplicate key is a parse error.
    Object(Vec<(String, Value)>),
}

/// Why a document was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Syntax,
    TooDeep,
    TooLong,
    Duplicate,
    Trailing,
}

impl Value {
    /// Member `key` of an object.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Int(n) if *n >= 0 => Some(*n as u64),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// Parse one whole document: `text` is a single value and nothing else.
pub fn parse(text: &str) -> Result<Value, Error> {
    let mut reader = Reader {
        bytes: text.as_bytes(),
        at: 0,
    };
    let value = reader.value(0)?;
    reader.space();
    if reader.at != reader.bytes.len() {
        return Err(Error::Trailing);
    }
    Ok(value)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn space(&mut self) {
        while matches!(self.bytes.get(self.at), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.at += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn eat(&mut self, byte: u8) -> Result<(), Error> {
        if self.peek() == Some(byte) {
            self.at += 1;
            Ok(())
        } else {
            Err(Error::Syntax)
        }
    }

    fn word(&mut self, word: &str, value: Value) -> Result<Value, Error> {
        if self.bytes[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            Err(Error::Syntax)
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value, Error> {
        self.space();
        match self.peek().ok_or(Error::Syntax)? {
            b'{' => self.object(depth),
            b'[' => self.array(depth),
            b'"' => self.string().map(Value::Str),
            b't' => self.word("true", Value::Bool(true)),
            b'f' => self.word("false", Value::Bool(false)),
            b'n' => self.word("null", Value::Null),
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(Error::Syntax),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, Error> {
        if depth >= MAX_DEPTH {
            return Err(Error::TooDeep);
        }
        self.eat(b'{')?;
        let mut members: Vec<(String, Value)> = Vec::new();
        self.space();
        if self.peek() == Some(b'}') {
            self.at += 1;
            return Ok(Value::Object(members));
        }
        loop {
            self.space();
            let key = self.string()?;
            self.space();
            self.eat(b':')?;
            let value = self.value(depth + 1)?;
            if members.iter().any(|(k, _)| *k == key) {
                return Err(Error::Duplicate);
            }
            if members.len() >= MAX_ELEMENTS {
                return Err(Error::TooLong);
            }
            members.push((key, value));
            self.space();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b'}') => {
                    self.at += 1;
                    return Ok(Value::Object(members));
                }
                _ => return Err(Error::Syntax),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, Error> {
        if depth >= MAX_DEPTH {
            return Err(Error::TooDeep);
        }
        self.eat(b'[')?;
        let mut items = Vec::new();
        self.space();
        if self.peek() == Some(b']') {
            self.at += 1;
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            if items.len() > MAX_ELEMENTS {
                return Err(Error::TooLong);
            }
            self.space();
            match self.peek() {
                Some(b',') => self.at += 1,
                Some(b']') => {
                    self.at += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(Error::Syntax),
            }
        }
    }

    fn number(&mut self) -> Result<Value, Error> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        let digits = self.at;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
        if self.at == digits {
            return Err(Error::Syntax);
        }
        // No leading zeros (`01`), as in the grammar.
        if self.bytes[digits] == b'0' && self.at - digits > 1 {
            return Err(Error::Syntax);
        }
        let mut float = false;
        if self.peek() == Some(b'.') {
            float = true;
            self.at += 1;
            let frac = self.at;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
            }
            if self.at == frac {
                return Err(Error::Syntax);
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            float = true;
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            let exp = self.at;
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.at += 1;
            }
            if self.at == exp {
                return Err(Error::Syntax);
            }
        }
        let text = core::str::from_utf8(&self.bytes[start..self.at]).map_err(|_| Error::Syntax)?;
        if !float {
            if let Ok(n) = text.parse::<i64>() {
                return Ok(Value::Int(n));
            }
        }
        text.parse::<f64>()
            .ok()
            .filter(|f| f.is_finite())
            .map(Value::Float)
            .ok_or(Error::Syntax)
    }

    fn string(&mut self) -> Result<String, Error> {
        self.eat(b'"')?;
        let mut out = String::new();
        loop {
            let byte = self.peek().ok_or(Error::Syntax)?;
            match byte {
                b'"' => {
                    self.at += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.at += 1;
                    let escape = self.peek().ok_or(Error::Syntax)?;
                    self.at += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode()?),
                        _ => return Err(Error::Syntax),
                    }
                }
                0..=0x1f => return Err(Error::Syntax),
                _ => {
                    // The input is a `&str`, so the bytes are valid UTF-8:
                    // copy one whole character.
                    let width = match byte {
                        0..=0x7f => 1,
                        0xc0..=0xdf => 2,
                        0xe0..=0xef => 3,
                        _ => 4,
                    };
                    let chunk = self
                        .bytes
                        .get(self.at..self.at + width)
                        .ok_or(Error::Syntax)?;
                    out.push_str(core::str::from_utf8(chunk).map_err(|_| Error::Syntax)?);
                    self.at += width;
                }
            }
            if out.len() > MAX_STRING {
                return Err(Error::TooLong);
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, Error> {
        let digits = self.bytes.get(self.at..self.at + 4).ok_or(Error::Syntax)?;
        let mut value = 0u32;
        for &digit in digits {
            let nibble = (digit as char).to_digit(16).ok_or(Error::Syntax)?;
            value = value << 4 | nibble;
        }
        self.at += 4;
        Ok(value)
    }

    fn unicode(&mut self) -> Result<char, Error> {
        let first = self.hex4()?;
        let code = if (0xd800..0xdc00).contains(&first) {
            // A high surrogate must be followed by `\u` and a low one.
            if self.bytes.get(self.at..self.at + 2) != Some(b"\\u") {
                return Err(Error::Syntax);
            }
            self.at += 2;
            let second = self.hex4()?;
            if !(0xdc00..0xe000).contains(&second) {
                return Err(Error::Syntax);
            }
            0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00)
        } else {
            first
        };
        char::from_u32(code).ok_or(Error::Syntax)
    }
}

/// Append `text` as a JSON string literal (quotes included) to `out`.
pub fn quote(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// A JSON string literal for `text`.
pub fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    quote(&mut out, text);
    out
}

/// Write `value` as compact JSON.
pub fn write(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(n) => {
            let _ = write!(out, "{n}");
        }
        Value::Float(f) => {
            let _ = write!(out, "{f}");
        }
        Value::Str(text) => quote(out, text),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write(out, item);
            }
            out.push(']');
        }
        Value::Object(members) => {
            out.push('{');
            for (index, (key, item)) in members.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                quote(out, key);
                out.push(':');
                write(out, item);
            }
            out.push('}');
        }
    }
}

/// Builds one JSON object by appending members, for the services' replies.
#[derive(Default)]
pub struct Object {
    text: String,
    members: usize,
}

impl Object {
    pub fn new() -> Object {
        Object::default()
    }

    fn key(&mut self, key: &str) {
        self.text.push(if self.members == 0 { '{' } else { ',' });
        self.members += 1;
        quote(&mut self.text, key);
        self.text.push(':');
    }

    pub fn str(mut self, key: &str, value: &str) -> Object {
        self.key(key);
        quote(&mut self.text, value);
        self
    }

    pub fn uint(mut self, key: &str, value: u64) -> Object {
        self.key(key);
        let _ = write!(self.text, "{value}");
        self
    }

    pub fn int(mut self, key: &str, value: i64) -> Object {
        self.key(key);
        let _ = write!(self.text, "{value}");
        self
    }

    pub fn bool(mut self, key: &str, value: bool) -> Object {
        self.key(key);
        self.text.push_str(if value { "true" } else { "false" });
        self
    }

    /// A member whose value is already JSON.
    pub fn raw(mut self, key: &str, json: &str) -> Object {
        self.key(key);
        self.text.push_str(json);
        self
    }

    pub fn finish(mut self) -> String {
        if self.members == 0 {
            self.text.push('{');
        }
        self.text.push('}');
        self.text
    }
}

/// `[a,b,c]` from already-rendered JSON elements.
pub fn array<I, S>(items: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out = String::from("[");
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(item.as_ref());
    }
    out.push(']');
    out
}
