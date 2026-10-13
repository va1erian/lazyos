//! Building one line: pure functions that write into a caller's buffer.
//!
//! The kernel has no `String` on some paths, so a builder takes a
//! `&mut dyn fmt::Write` (usually a [`LineBuf`] on the stack) and allocates
//! nothing. Every builder keeps the one-line rule: control characters in a
//! detail or a value become spaces, so a line is always exactly one line, and
//! a field value that holds a space or a quote is written the way
//! [`crate::line::parse`] reads it back.

use core::fmt::{self, Display, Write};

use crate::Channel;

/// A fixed buffer a line is built in; a line past `N` bytes is cut (its
/// newline is kept) and [`LineBuf::truncated`] says so.
pub struct LineBuf<const N: usize> {
    bytes: [u8; N],
    len: usize,
    truncated: bool,
}

impl<const N: usize> LineBuf<N> {
    pub const fn new() -> Self {
        LineBuf {
            bytes: [0; N],
            len: 0,
            truncated: false,
        }
    }

    /// The bytes written so far.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// Whether a write did not fit.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// End the line: a trailing newline, always present (it replaces the last
    /// byte of a line that filled the buffer). A line that was cut ends in
    /// `...`, so a reader can tell a long panic message from a short one.
    /// Returns the whole line.
    pub fn finish(&mut self) -> &[u8] {
        if self.len == N {
            self.len = N - 1;
            self.truncated = true;
        }
        if self.truncated && self.len >= 3 {
            // Back up to a character boundary, so the dots never split one.
            let mut end = self.len - 3;
            while end > 0 && self.bytes[end] & 0xC0 == 0x80 {
                end -= 1;
            }
            self.bytes[end..end + 3].copy_from_slice(b"...");
            self.len = end + 3;
        }
        self.bytes[self.len] = b'\n';
        self.len += 1;
        &self.bytes[..self.len]
    }

    /// Start over.
    pub fn clear(&mut self) {
        self.len = 0;
        self.truncated = false;
    }
}

impl<const N: usize> Default for LineBuf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Write for LineBuf<N> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        // One byte stays free for the newline `finish` adds.
        let room = (N - 1).saturating_sub(self.len);
        let mut take = text.len().min(room);
        while !text.is_char_boundary(take) {
            take -= 1;
        }
        self.bytes[self.len..self.len + take].copy_from_slice(&text.as_bytes()[..take]);
        self.len += take;
        if take < text.len() {
            self.truncated = true;
        }
        Ok(())
    }
}

/// Passes text through with every control character turned into a space.
struct Flatten<'a>(&'a mut dyn Write);

impl Write for Flatten<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let mut rest = text;
        while let Some(at) = rest.find(char::is_control) {
            self.0.write_str(&rest[..at])?;
            self.0.write_char(' ')?;
            rest = &rest[at + rest[at..].chars().next().map_or(1, char::len_utf8)..];
        }
        self.0.write_str(rest)
    }
}

/// Learns whether a value needs quoting without storing it.
#[derive(Default)]
struct NeedsQuote(bool);

impl Write for NeedsQuote {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0 |= text
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c.is_control());
        Ok(())
    }
}

/// A quoted value: embedded quotes become `'` (the parser ends a quoted
/// value at the next `"`), control characters become spaces.
struct Quoted<'a>(&'a mut dyn Write);

impl Write for Quoted<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        for c in text.chars() {
            self.0.write_char(match c {
                '"' => '\'',
                c if c.is_control() => ' ',
                c => c,
            })?;
        }
        Ok(())
    }
}

/// `PREFIX:` (a channel's prefix and its colon).
fn open(out: &mut dyn Write, channel: Channel) -> fmt::Result {
    out.write_str(channel.prefix())?;
    out.write_char(':')
}

/// `PREFIX:STAGE:PASS`.
pub fn pass(out: &mut dyn Write, channel: Channel, stage: &str) -> fmt::Result {
    open(out, channel)?;
    out.write_str(stage)?;
    out.write_str(":PASS")
}

/// `PREFIX:STAGE:PASS:detail`, the detail flattened to one line.
pub fn pass_with(
    out: &mut dyn Write,
    channel: Channel,
    stage: &str,
    detail: fmt::Arguments<'_>,
) -> fmt::Result {
    status(out, channel, stage, "PASS", detail)
}

/// `PREFIX:STAGE:FAIL:detail`.
pub fn fail(
    out: &mut dyn Write,
    channel: Channel,
    stage: &str,
    detail: fmt::Arguments<'_>,
) -> fmt::Result {
    status(out, channel, stage, "FAIL", detail)
}

/// `PREFIX:STAGE:WARN:detail`.
pub fn warn(
    out: &mut dyn Write,
    channel: Channel,
    stage: &str,
    detail: fmt::Arguments<'_>,
) -> fmt::Result {
    status(out, channel, stage, "WARN", detail)
}

fn status(
    out: &mut dyn Write,
    channel: Channel,
    stage: &str,
    verdict: &str,
    detail: fmt::Arguments<'_>,
) -> fmt::Result {
    open(out, channel)?;
    out.write_str(stage)?;
    out.write_char(':')?;
    out.write_str(verdict)?;
    out.write_char(':')?;
    Flatten(out).write_fmt(detail)
}

/// `PREFIX:name k=v k="a b"`: an event with fields. A key is
/// `[A-Za-z0-9_.-]+` (it is a code identifier at every call site).
pub fn event(
    out: &mut dyn Write,
    channel: Channel,
    name: &str,
    fields: &[(&str, &dyn Display)],
) -> fmt::Result {
    open(out, channel)?;
    out.write_str(name)?;
    for (key, value) in fields {
        out.write_char(' ')?;
        out.write_str(key)?;
        out.write_char('=')?;
        field_value(out, *value)?;
    }
    Ok(())
}

/// One field value, quoted when it needs it.
fn field_value(out: &mut dyn Write, value: &dyn Display) -> fmt::Result {
    let mut probe = NeedsQuote::default();
    write!(probe, "{value}")?;
    if probe.0 {
        out.write_char('"')?;
        write!(Quoted(out), "{value}")?;
        out.write_char('"')
    } else {
        write!(out, "{value}")
    }
}

/// `PREFIX:` and the formatted text, flattened (the escape hatch for a
/// line whose shape is its own, `BOOT:PHASE:<name>:tsc=<n>`).
pub fn line(out: &mut dyn Write, channel: Channel, text: fmt::Arguments<'_>) -> fmt::Result {
    open(out, channel)?;
    Flatten(out).write_fmt(text)
}

/// `PREFIX:SUPPRESSED n=<count>`: what a channel over its budget prints once
/// per window.
pub fn suppressed(out: &mut dyn Write, channel: Channel, count: u32) -> fmt::Result {
    open(out, channel)?;
    write!(out, "SUPPRESSED n={count}")
}
