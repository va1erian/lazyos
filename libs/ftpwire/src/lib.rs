//! The FTP client's wire format (docs/networking-plan.md, stage N4).
//!
//! Everything the `ftp` tool parses or builds that a server (or a hostile
//! network) can influence lives here, pure and host tested:
//!
//! * [`ReplyParser`]: the control connection's replies, fed bytes in any
//!   chunking, with a multi-line reply (`123-` ... `123 `) returned whole. Line,
//!   reply and total sizes are bounded; a reply that breaks the format is an
//!   error, never a guess.
//! * [`parse_pasv`] / [`parse_epsv`]: the data connection's address from a `227`
//!   or `229` reply. The tool connects to the *control* peer's address and
//!   takes only the port (a server must not be able to send the client to a
//!   third host: the "FTP bounce" in reverse), so the address is parsed to be
//!   validated, not obeyed.
//! * [`command`]: a request line, refusing an argument that could carry a
//!   second command (CR, LF, NUL) or is absurdly long.
//! * [`crc32`]: the checksum the tool prints for a transfer, so the harness can
//!   compare bytes it never saw.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

/// Longest reply line accepted, bytes (RFC 959 sets no limit; real servers
/// stay far under this).
pub const MAX_LINE: usize = 1024;
/// Lines in one multi-line reply.
pub const MAX_LINES: usize = 64;
/// Longest argument of a command we build.
pub const MAX_ARG: usize = 512;

/// A complete reply: the three-digit code and the text of every line (without
/// the code, the separator or the line ending).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub code: u16,
    pub lines: Vec<String>,
}

impl Reply {
    /// The text of the last line, which carries the message for a one-line
    /// reply.
    pub fn text(&self) -> &str {
        self.lines.last().map_or("", String::as_str)
    }

    /// First digit: 1 preliminary, 2 success, 3 needs more, 4 transient
    /// failure, 5 permanent failure.
    pub fn class(&self) -> u16 {
        self.code / 100
    }
}

/// Why a reply could not be parsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyError {
    /// A line does not start with a three-digit code and a space or hyphen.
    BadLine,
    /// A line longer than [`MAX_LINE`].
    LineTooLong,
    /// More than [`MAX_LINES`] lines in one reply.
    TooManyLines,
    /// A line inside a multi-line reply carries another code than the first
    /// (continuation lines may carry none, but not a different one).
    CodeChanged,
    /// The code is outside 100..=599.
    BadCode,
}

/// Incremental parser for the control connection. Feed it whatever arrives;
/// call [`ReplyParser::next`] until it returns `None`.
#[derive(Default)]
pub struct ReplyParser {
    /// Bytes of the line being read.
    line: Vec<u8>,
    /// The reply being assembled: its code and lines so far.
    code: Option<u16>,
    lines: Vec<String>,
    /// Replies complete but not yet taken.
    done: Vec<Reply>,
    /// The first error; the parser is dead after it.
    failed: Option<ReplyError>,
    /// A CR was the last byte (so the LF that follows ends the line).
    saw_cr: bool,
}

impl ReplyParser {
    pub fn new() -> ReplyParser {
        ReplyParser::default()
    }

    /// Feed bytes. Returns the error if the stream is malformed (it stays
    /// failed: a control connection that lost sync cannot be trusted again).
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), ReplyError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        for &byte in bytes {
            let result = self.byte(byte);
            if let Err(error) = result {
                self.failed = Some(error);
                return Err(error);
            }
        }
        Ok(())
    }

    /// The next complete reply, if one has arrived.
    #[allow(clippy::should_implement_trait)] // not an Iterator: it also reports errors elsewhere
    pub fn next(&mut self) -> Option<Reply> {
        (!self.done.is_empty()).then(|| self.done.remove(0))
    }

    /// Bytes buffered for a line not yet complete (bounded by [`MAX_LINE`]).
    pub fn pending(&self) -> usize {
        self.line.len()
    }

    fn byte(&mut self, byte: u8) -> Result<(), ReplyError> {
        match byte {
            b'\r' => {
                self.saw_cr = true;
                Ok(())
            }
            b'\n' => {
                self.saw_cr = false;
                self.end_line()
            }
            _ => {
                // A bare CR in the middle of a line is part of the text of a
                // broken server; refuse it rather than reinterpret it.
                if self.saw_cr {
                    return Err(ReplyError::BadLine);
                }
                if self.line.len() >= MAX_LINE {
                    return Err(ReplyError::LineTooLong);
                }
                self.line.push(byte);
                Ok(())
            }
        }
    }

    fn end_line(&mut self) -> Result<(), ReplyError> {
        let line = core::mem::take(&mut self.line);
        let (code, sep, text) = split_line(&line, self.code)?;
        match self.code {
            None => {
                let code = code.ok_or(ReplyError::BadLine)?;
                if !(100..=599).contains(&code) {
                    return Err(ReplyError::BadCode);
                }
                self.lines.push(text);
                if sep == b' ' {
                    self.finish(code);
                } else {
                    self.code = Some(code);
                }
            }
            Some(first) => {
                if let Some(code) = code {
                    if code != first {
                        return Err(ReplyError::CodeChanged);
                    }
                }
                if self.lines.len() >= MAX_LINES {
                    return Err(ReplyError::TooManyLines);
                }
                self.lines.push(text);
                // The reply ends on its own code followed by a space.
                if code == Some(first) && sep == b' ' {
                    self.code = None;
                    self.finish(first);
                }
            }
        }
        Ok(())
    }

    fn finish(&mut self, code: u16) {
        self.code = None;
        self.done.push(Reply {
            code,
            lines: core::mem::take(&mut self.lines),
        });
    }
}

/// Split `NNN<sep>text`; inside a multi-line reply a line without a code is
/// text (`code` is `None`, `sep` a hyphen).
fn split_line(line: &[u8], open: Option<u16>) -> Result<(Option<u16>, u8, String), ReplyError> {
    let has_code = line.len() >= 3 && line[..3].iter().all(u8::is_ascii_digit);
    let separator = line.get(3).copied();
    if has_code && matches!(separator, Some(b' ') | Some(b'-') | None) {
        let code = u16::from(line[0] - b'0') * 100
            + u16::from(line[1] - b'0') * 10
            + u16::from(line[2] - b'0');
        // A bare code ("250") is a reply with no text.
        let sep = separator.unwrap_or(b' ');
        let text = line.get(4..).unwrap_or(&[]);
        return Ok((Some(code), sep, lossy(text)));
    }
    if open.is_some() {
        return Ok((None, b'-', lossy(line)));
    }
    Err(ReplyError::BadLine)
}

/// Text from bytes: anything that is not printable ASCII becomes `?`, so a
/// reply can never smuggle control characters into the console.
fn lossy(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| {
            if (0x20..0x7F).contains(&b) {
                char::from(b)
            } else {
                '?'
            }
        })
        .collect()
}

/// The `(h1,h2,h3,h4,p1,p2)` of a `227` reply: the address and port. Each
/// field must be a decimal 0 to 255 with no sign or spaces inside; the first
/// well-formed group in the text is used.
pub fn parse_pasv(text: &str) -> Option<([u8; 4], u16)> {
    let open = text.find('(')?;
    let close = open + text[open..].find(')')?;
    let mut fields = [0u8; 6];
    let mut parts = text[open + 1..close].split(',');
    for field in &mut fields {
        let part = parts.next()?;
        if part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        *field = u8::try_from(part.parse::<u16>().ok()?).ok()?;
    }
    if parts.next().is_some() {
        return None;
    }
    let port = u16::from(fields[4]) << 8 | u16::from(fields[5]);
    (port != 0).then_some(([fields[0], fields[1], fields[2], fields[3]], port))
}

/// The port of a `229` reply, `(|||port|)` (any delimiter character, empty
/// protocol and address fields).
pub fn parse_epsv(text: &str) -> Option<u16> {
    let open = text.find('(')?;
    let close = open + text[open..].find(')')?;
    let inner = &text[open + 1..close];
    let delimiter = inner.chars().next()?;
    if delimiter.is_ascii_alphanumeric() || !delimiter.is_ascii_graphic() {
        return None;
    }
    let mut parts = inner.split(delimiter);
    // "", "", "", port, ""
    let fields: Vec<&str> = parts.by_ref().collect();
    if fields.len() != 5 || !fields[0].is_empty() || !fields[1].is_empty() || !fields[2].is_empty()
    {
        return None;
    }
    if !fields[4].is_empty() {
        return None;
    }
    let port = fields[3];
    if port.is_empty() || port.len() > 5 || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    port.parse::<u16>().ok().filter(|p| *p != 0)
}

/// Why a command could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandError {
    /// The argument holds CR, LF or NUL, which would end the line or inject a
    /// command.
    BadCharacter,
    /// The argument is longer than [`MAX_ARG`].
    TooLong,
    /// The verb is not 3 or 4 ASCII letters.
    BadVerb,
}

/// `VERB[ argument]\r\n`.
pub fn command(verb: &str, argument: Option<&str>) -> Result<Vec<u8>, CommandError> {
    if !(3..=4).contains(&verb.len()) || !verb.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(CommandError::BadVerb);
    }
    let mut line = Vec::with_capacity(verb.len() + 2 + argument.map_or(0, str::len));
    line.extend_from_slice(verb.as_bytes());
    if let Some(argument) = argument {
        if argument.len() > MAX_ARG {
            return Err(CommandError::TooLong);
        }
        if argument.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
            return Err(CommandError::BadCharacter);
        }
        line.push(b' ');
        line.extend_from_slice(argument.as_bytes());
    }
    line.extend_from_slice(b"\r\n");
    Ok(line)
}

/// IEEE CRC-32 (the zlib/PNG polynomial), updated incrementally:
/// `crc32_update(0, a)` then `crc32_update(that, b)` equals `crc32(a ++ b)`.
pub fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    let mut crc = !crc;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (!(crc & 1)).wrapping_add(1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// CRC-32 of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    crc32_update(0, data)
}

/// The deterministic byte stream `nc -g` and `ftp put -g` send: an xorshift64
/// generator, so a peer can regenerate it to compare (the harness does, in
/// `tools/net/hostpeers.py`).
pub struct Pattern(u64);

impl Default for Pattern {
    fn default() -> Pattern {
        Pattern(0x9E37_79B9_7F4A_7C15)
    }
}

impl Pattern {
    pub fn new() -> Pattern {
        Pattern::default()
    }

    pub fn fill(&mut self, out: &mut [u8]) {
        for byte in out {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            *byte = (self.0 >> 24) as u8;
        }
    }
}

#[cfg(test)]
mod tests;
