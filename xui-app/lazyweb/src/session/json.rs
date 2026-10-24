//! A small, strict JSON reader: only what a hostile file could use to waste time or memory is
//! bounded (depth and size are capped by the caller and [`MAX_DEPTH`]).

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
