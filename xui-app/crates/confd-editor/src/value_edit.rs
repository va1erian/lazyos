//! Parsing and formatting one [`confd::Value`] per kind.
//!
//! The editor keeps a value as text while the user types; this module is the
//! pure grammar between that text and the typed value. The size limits are the
//! service's own ([`confd::MAX_VALUE_LEN`]) and are measured in **bytes**, not
//! characters, so a multibyte string is rejected at the same point `confd`
//! would reject it.

use std::num::IntErrorKind;

use confd::{Value, MAX_VALUE_LEN};

/// The five value kinds `confd` stores, in the order the kind picker shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bool,
    I64,
    U64,
    Str,
    Bytes,
}

impl Kind {
    /// Every kind, in picker order.
    pub const ALL: [Kind; 5] = [Kind::Bool, Kind::I64, Kind::U64, Kind::Str, Kind::Bytes];

    /// The label the kind picker shows.
    pub const fn label(self) -> &'static str {
        match self {
            Kind::Bool => "bool",
            Kind::I64 => "i64",
            Kind::U64 => "u64",
            Kind::Str => "string",
            Kind::Bytes => "bytes",
        }
    }

    /// The kind's index in [`Kind::ALL`], for a `RadioGroup`.
    pub fn index(self) -> usize {
        Kind::ALL.iter().position(|kind| *kind == self).unwrap_or(0)
    }

    /// The kind at `index`, defaulting to the first when out of range.
    pub fn from_index(index: usize) -> Kind {
        Kind::ALL.get(index).copied().unwrap_or(Kind::Bool)
    }

    /// The kind of a stored value.
    pub fn from_value(value: &Value) -> Kind {
        match value {
            Value::Bool(_) => Kind::Bool,
            Value::I64(_) => Kind::I64,
            Value::U64(_) => Kind::U64,
            Value::Str(_) => Kind::Str,
            Value::Bytes(_) => Kind::Bytes,
        }
    }
}

/// Formats a value as editable text (bytes as spaced lowercase hex).
pub fn format(value: &Value) -> String {
    match value {
        Value::Bool(flag) => if *flag { "true" } else { "false" }.to_owned(),
        Value::I64(number) => number.to_string(),
        Value::U64(number) => number.to_string(),
        Value::Str(text) => escape(text),
        Value::Bytes(bytes) => bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// Parses `text` as `kind`, or returns a message for the status line.
pub fn parse(kind: Kind, text: &str) -> Result<Value, String> {
    match kind {
        Kind::Bool => match text.trim().to_ascii_lowercase().as_str() {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err("a bool must be true or false".into()),
        },
        Kind::I64 => parse_i64(text),
        Kind::U64 => parse_u64(text),
        Kind::Str => {
            let text = unescape(text)?;
            if text.len() > MAX_VALUE_LEN {
                Err(format!(
                    "the string is {} bytes; the limit is {MAX_VALUE_LEN}",
                    text.len()
                ))
            } else {
                Ok(Value::Str(text))
            }
        }
        Kind::Bytes => parse_bytes(text),
    }
}

/// The longest preview, in characters, before it is cut with an ellipsis.
const PREVIEW_CHARS: usize = 48;

/// Escapes backslashes and control characters so a multi-line string (such as
/// `sys/ui/menu`) stays on the single line of an `Edit`. [`unescape`] is the
/// inverse, so what the user sees is exactly what is stored.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Reverses [`escape`]; an unknown or unfinished escape is an error rather
/// than being stored by accident.
fn unescape(text: &str) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('u') => out.push(unicode_escape(&mut chars)?),
            Some(other) => {
                return Err(format!(
                    "unknown escape \\{other}; write \\\\ for a backslash"
                ))
            }
            None => return Err("a string cannot end in a lone backslash (use \\\\)".into()),
        }
    }
    Ok(out)
}

/// Parses the `{hex}` after `\u` in a string escape.
fn unicode_escape(chars: &mut std::str::Chars<'_>) -> Result<char, String> {
    let bad = || "a \\u escape looks like \\u{1f}".to_owned();
    if chars.next() != Some('{') {
        return Err(bad());
    }
    let mut digits = String::new();
    loop {
        match chars.next() {
            Some('}') => break,
            Some(c) if c.is_ascii_hexdigit() && digits.len() < 6 => digits.push(c),
            _ => return Err(bad()),
        }
    }
    u32::from_str_radix(&digits, 16)
        .ok()
        .and_then(char::from_u32)
        .ok_or_else(bad)
}

/// Cuts `text` to [`PREVIEW_CHARS`] characters (never mid-character).
fn truncate(text: String) -> String {
    match text.char_indices().nth(PREVIEW_CHARS) {
        Some((end, _)) => format!("{}\u{2026}", &text[..end]),
        None => text,
    }
}

/// A one-line rendering of a stored value for the preview label.
///
/// Strings are escaped and long ones truncated, so a multi-line value cannot
/// spill over the controls below the label. A byte string that is not UTF-8
/// is shown as hex with a note, so an opaque value is never silently rendered
/// as replacement characters.
pub fn preview(value: &Value) -> String {
    truncate(match value {
        Value::Str(text) if text.is_empty() => "(empty string)".into(),
        Value::Str(text) => escape(text),
        Value::Bytes(bytes) if bytes.is_empty() => "(empty bytes)".into(),
        Value::Bytes(bytes) => match std::str::from_utf8(bytes) {
            Ok(text) => escape(text),
            Err(_) => format!("{} ({} bytes, not UTF-8)", format(value), bytes.len()),
        },
        other => format(other),
    })
}

fn parse_i64(text: &str) -> Result<Value, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("enter an integer".into());
    }
    if trimmed.starts_with("0x") || trimmed.starts_with("0X") {
        return Err("i64 is decimal; use the u64 kind for hex".into());
    }
    trimmed
        .parse::<i64>()
        .map(Value::I64)
        .map_err(|error| int_error("i64", &error))
}

fn parse_u64(text: &str) -> Result<Value, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("enter an integer".into());
    }
    let (digits, radix) = match trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        Some(rest) => (rest, 16),
        None => (trimmed, 10),
    };
    if digits.is_empty() {
        return Err("enter an integer".into());
    }
    u64::from_str_radix(digits, radix)
        .map(Value::U64)
        .map_err(|error| int_error("u64", &error))
}

fn parse_bytes(text: &str) -> Result<Value, String> {
    let digits: Vec<u8> = text
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if !digits.len().is_multiple_of(2) {
        return Err("hex bytes need an even number of digits".into());
    }
    let mut out = Vec::with_capacity(digits.len() / 2);
    for pair in digits.chunks(2) {
        let high = hex_value(pair[0]).ok_or_else(|| not_hex(pair[0]))?;
        let low = hex_value(pair[1]).ok_or_else(|| not_hex(pair[1]))?;
        out.push((high << 4) | low);
    }
    if out.len() > MAX_VALUE_LEN {
        return Err(format!(
            "the byte string is {} bytes; the limit is {MAX_VALUE_LEN}",
            out.len()
        ));
    }
    Ok(Value::Bytes(out))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn not_hex(byte: u8) -> String {
    format!("'{}' is not a hex digit", char::from(byte))
}

fn int_error(what: &str, error: &std::num::ParseIntError) -> String {
    match error.kind() {
        IntErrorKind::Empty => format!("enter a {what} value"),
        IntErrorKind::PosOverflow | IntErrorKind::NegOverflow => {
            format!("the value is out of range for {what}")
        }
        _ => format!("that is not a valid {what} value"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_accepts_only_true_and_false() {
        assert_eq!(parse(Kind::Bool, "true"), Ok(Value::Bool(true)));
        assert_eq!(parse(Kind::Bool, " TRUE "), Ok(Value::Bool(true)));
        assert_eq!(parse(Kind::Bool, "False"), Ok(Value::Bool(false)));
        assert!(parse(Kind::Bool, "1").is_err());
        assert!(parse(Kind::Bool, "").is_err());
    }

    #[test]
    fn i64_is_decimal_with_sign_and_range_checks() {
        assert_eq!(parse(Kind::I64, "-5"), Ok(Value::I64(-5)));
        assert_eq!(parse(Kind::I64, "0"), Ok(Value::I64(0)));
        assert_eq!(
            parse(Kind::I64, "9223372036854775807"),
            Ok(Value::I64(i64::MAX))
        );
        assert!(parse(Kind::I64, "9223372036854775808").is_err());
        assert!(parse(Kind::I64, "").is_err());
        assert!(parse(Kind::I64, "12x").is_err());
        assert!(parse(Kind::I64, "0x10").is_err());
    }

    #[test]
    fn u64_is_decimal_or_hex_with_range_checks() {
        assert_eq!(parse(Kind::U64, "255"), Ok(Value::U64(255)));
        assert_eq!(parse(Kind::U64, "0xff"), Ok(Value::U64(255)));
        assert_eq!(parse(Kind::U64, "0XFF"), Ok(Value::U64(255)));
        assert_eq!(
            parse(Kind::U64, "18446744073709551615"),
            Ok(Value::U64(u64::MAX))
        );
        assert!(parse(Kind::U64, "18446744073709551616").is_err());
        assert!(parse(Kind::U64, "-1").is_err());
        assert!(parse(Kind::U64, "0x").is_err());
        assert!(parse(Kind::U64, "").is_err());
    }

    #[test]
    fn strings_are_limited_in_bytes_not_chars() {
        let ascii_ok = "a".repeat(MAX_VALUE_LEN);
        assert_eq!(
            parse(Kind::Str, &ascii_ok),
            Ok(Value::Str(ascii_ok.clone()))
        );
        assert!(parse(Kind::Str, &"a".repeat(MAX_VALUE_LEN + 1)).is_err());
        // 2048 two-byte characters are exactly the 4 KiB limit.
        let multibyte_ok = "\u{e9}".repeat(MAX_VALUE_LEN / 2);
        assert!(parse(Kind::Str, &multibyte_ok).is_ok());
        // One more character crosses it, though the char count is tiny.
        let multibyte_over = "\u{e9}".repeat(MAX_VALUE_LEN / 2 + 1);
        assert!(parse(Kind::Str, &multibyte_over).is_err());
    }

    #[test]
    fn bytes_are_hex_tolerant_of_spaces() {
        assert_eq!(
            parse(Kind::Bytes, "01 02 ff"),
            Ok(Value::Bytes(vec![1, 2, 0xff]))
        );
        assert_eq!(
            parse(Kind::Bytes, "0a0B"),
            Ok(Value::Bytes(vec![0x0a, 0x0b]))
        );
        assert_eq!(parse(Kind::Bytes, ""), Ok(Value::Bytes(Vec::new())));
        assert!(parse(Kind::Bytes, "0").is_err());
        assert!(parse(Kind::Bytes, "zz").is_err());
        assert!(parse(Kind::Bytes, "0g").is_err());
    }

    #[test]
    fn format_round_trips_every_kind() {
        for value in [
            Value::Bool(true),
            Value::I64(-5),
            Value::U64(0xff),
            Value::Str("dark".into()),
            Value::Bytes(vec![1, 0xff]),
        ] {
            let text = format(&value);
            assert_eq!(parse(Kind::from_value(&value), &text), Ok(value));
        }
        assert_eq!(format(&Value::Bytes(vec![1, 0xff])), "01 ff");
    }

    #[test]
    fn multiline_strings_round_trip_through_the_edit_text() {
        let value = Value::Str("terminal\tTerminal\nsys\\mon\r\u{1}".into());
        let text = format(&value);
        assert!(!text.contains('\n') && !text.contains('\t'));
        assert_eq!(text, "terminal\\tTerminal\\nsys\\\\mon\\r\\u{1}");
        assert_eq!(parse(Kind::Str, &text), Ok(value));
    }

    #[test]
    fn bad_string_escapes_are_errors_not_stored() {
        for bad in ["a\\", "a\\q", "\\u12", "\\u{zz}", "\\u{110000}", "\\u{}"] {
            assert!(parse(Kind::Str, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_byte_limit_applies_after_unescaping() {
        // 4096 escaped newlines are 8192 characters but only 4096 bytes.
        assert!(parse(Kind::Str, &"\\n".repeat(MAX_VALUE_LEN)).is_ok());
        assert!(parse(Kind::Str, &"\\n".repeat(MAX_VALUE_LEN + 1)).is_err());
    }

    #[test]
    fn preview_is_one_short_line() {
        let shown = preview(&Value::Str("line\n".repeat(100)));
        assert!(!shown.contains('\n'));
        assert_eq!(shown.chars().count(), PREVIEW_CHARS + 1);
        assert!(shown.ends_with('\u{2026}'));
        // Truncation lands on a character boundary.
        assert!(preview(&Value::Str("\u{e9}".repeat(200))).ends_with('\u{2026}'));
    }

    #[test]
    fn preview_flags_non_utf8_bytes() {
        assert_eq!(preview(&Value::Str("hi".into())), "hi");
        assert_eq!(preview(&Value::Bytes(b"hi".to_vec())), "hi");
        assert_eq!(preview(&Value::Str(String::new())), "(empty string)");
        let shown = preview(&Value::Bytes(vec![0xff, 0xfe]));
        assert!(shown.contains("not UTF-8"), "{shown}");
        assert!(shown.contains("ff fe"), "{shown}");
    }

    #[test]
    fn kind_index_and_value_kind_round_trip() {
        let samples = [
            Value::Bool(true),
            Value::I64(-1),
            Value::U64(1),
            Value::Str("x".into()),
            Value::Bytes(vec![0]),
        ];
        for value in samples {
            let kind = Kind::from_value(&value);
            assert_eq!(Kind::from_index(kind.index()), kind);
        }
        assert_eq!(Kind::from_index(99), Kind::Bool);
    }
}
