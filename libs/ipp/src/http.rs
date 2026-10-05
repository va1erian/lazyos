//! IPP over HTTP/1.1 (RFC 8010 4): a `POST` of `application/ipp` whose body
//! is the encoded request followed by the document, sent chunked so a
//! document of unknown length streams as it is made, and a bounded reader
//! for the reply.
//!
//! Each request uses its own connection (`Connection: close`): a print client
//! sends a handful of requests per job, and a reply then ends where the
//! connection does when the printer gives no length.

use std::io::{self, Read, Write};
use std::string::String;
use std::vec::Vec;

use crate::Message;

/// Largest reply head (status line and headers) read.
const MAX_HEAD: usize = 16 * 1024;
/// Largest reply body read: a full Get-Printer-Attributes reply is ~30 KB.
pub const MAX_BODY: usize = 1024 * 1024;

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, String::from(what))
}

/// A request body being sent, one chunk per [`Post::write`].
pub struct Post<'s, S: Write> {
    stream: &'s mut S,
}

impl<'s, S: Write> Post<'s, S> {
    /// Sends the request head for `path` on `host` (the `Host` header,
    /// `host:port`). Both come from a checked [`crate::uri::PrinterUri`];
    /// a CR or LF in either is refused rather than sent.
    pub fn start(stream: &'s mut S, host: &str, path: &str) -> io::Result<Post<'s, S>> {
        if [host, path]
            .iter()
            .any(|s| s.bytes().any(|b| b < 0x20 || b == 0x7F))
        {
            return Err(invalid("control character in the printer address"));
        }
        let head = std::format!(
            "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/ipp\r\n\
             Transfer-Encoding: chunked\r\nConnection: close\r\nUser-Agent: LazyOS-print/0.1\r\n\r\n"
        );
        stream.write_all(head.as_bytes())?;
        Ok(Post { stream })
    }

    /// Sends `data` as one chunk (nothing for empty data: an empty chunk
    /// would end the body).
    pub fn write(&mut self, data: &[u8]) -> io::Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        write!(self.stream, "{:x}\r\n", data.len())?;
        self.stream.write_all(data)?;
        self.stream.write_all(b"\r\n")
    }

    /// Ends the body.
    pub fn finish(self) -> io::Result<()> {
        self.stream.write_all(b"0\r\n\r\n")?;
        self.stream.flush()
    }
}

/// Reads an HTTP reply and returns its body when the status is 200. Interim
/// `1xx` replies are skipped; anything else is an error naming the status.
pub fn read_reply<S: Read>(stream: &mut S) -> io::Result<Vec<u8>> {
    let mut buffered = Vec::new();
    loop {
        let (status, head, rest) = read_head(stream, buffered)?;
        if (100..200).contains(&status) {
            buffered = rest;
            continue;
        }
        if status != 200 {
            return Err(io::Error::other(std::format!(
                "the printer answered HTTP {status}"
            )));
        }
        return read_body(stream, &head, rest);
    }
}

/// The status, the header lines and the bytes read past the head.
fn read_head<S: Read>(stream: &mut S, mut buf: Vec<u8>) -> io::Result<(u16, String, Vec<u8>)> {
    let mut chunk = [0u8; 2048];
    let end = loop {
        if let Some(at) = find(&buf, b"\r\n\r\n") {
            break at;
        }
        if buf.len() > MAX_HEAD {
            return Err(invalid("reply head too long"));
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the printer closed the connection",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = std::str::from_utf8(&buf[..end]).map_err(|_| invalid("reply head is not text"))?;
    let status_line = head.lines().next().unwrap_or_default();
    let mut parts = status_line.split(' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err(invalid("not an HTTP reply"));
    }
    let status = parts
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| invalid("bad HTTP status line"))?;
    let head = String::from(head);
    let rest = buf[end + 4..].to_vec();
    Ok((status, head, rest))
}

fn header<'h>(head: &'h str, name: &str) -> Option<&'h str> {
    head.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
    })
}

fn read_body<S: Read>(stream: &mut S, head: &str, rest: Vec<u8>) -> io::Result<Vec<u8>> {
    let chunked = header(head, "transfer-encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
    let length = header(head, "content-length")
        .map(|v| {
            v.parse::<usize>()
                .map_err(|_| invalid("bad Content-Length"))
        })
        .transpose()?;
    if chunked {
        return dechunk(stream, rest);
    }
    let mut body = rest;
    match length {
        Some(length) if length > MAX_BODY => Err(invalid("reply too large")),
        Some(length) => {
            if body.len() < length {
                let mut more = std::vec![0u8; length - body.len()];
                stream.read_exact(&mut more)?;
                body.extend_from_slice(&more);
            }
            body.truncate(length);
            Ok(body)
        }
        None => {
            stream
                .take((MAX_BODY + 1 - body.len().min(MAX_BODY)) as u64)
                .read_to_end(&mut body)?;
            if body.len() > MAX_BODY {
                return Err(invalid("reply too large"));
            }
            Ok(body)
        }
    }
}

/// A chunked body, `rest` being what was already read past the head.
fn dechunk<S: Read>(stream: &mut S, rest: Vec<u8>) -> io::Result<Vec<u8>> {
    let mut input = Buffered {
        stream,
        buf: rest,
        at: 0,
    };
    let mut body = Vec::new();
    loop {
        let line = input.line()?;
        let size = line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size, 16).map_err(|_| invalid("bad chunk size"))?;
        if size == 0 {
            return Ok(body);
        }
        if body.len().checked_add(size).is_none_or(|n| n > MAX_BODY) {
            return Err(invalid("reply too large"));
        }
        body.extend_from_slice(&input.take(size)?);
        if !input.line()?.is_empty() {
            return Err(invalid("chunk not followed by CRLF"));
        }
    }
}

struct Buffered<'a, S: Read> {
    stream: &'a mut S,
    buf: Vec<u8>,
    at: usize,
}

impl<S: Read> Buffered<'_, S> {
    fn fill(&mut self) -> io::Result<()> {
        let mut chunk = [0u8; 4096];
        let n = self.stream.read(&mut chunk)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "reply cut short",
            ));
        }
        self.buf.drain(..self.at);
        self.at = 0;
        self.buf.extend_from_slice(&chunk[..n]);
        Ok(())
    }

    fn line(&mut self) -> io::Result<String> {
        loop {
            if let Some(end) = find(&self.buf[self.at..], b"\r\n") {
                let line = &self.buf[self.at..self.at + end];
                let line = String::from_utf8_lossy(line).into_owned();
                self.at += end + 2;
                return Ok(line);
            }
            if self.buf.len() - self.at > 1024 {
                return Err(invalid("chunk header too long"));
            }
            self.fill()?;
        }
    }

    fn take(&mut self, n: usize) -> io::Result<Vec<u8>> {
        while self.buf.len() - self.at < n {
            self.fill()?;
        }
        let out = self.buf[self.at..self.at + n].to_vec();
        self.at += n;
        Ok(out)
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Sends `request` with no document and decodes the reply.
pub fn exchange<S: Read + Write>(
    stream: &mut S,
    host: &str,
    path: &str,
    request: &Message,
) -> io::Result<Message> {
    let bytes = request
        .encode()
        .map_err(|e| invalid(&std::format!("request not encodable: {e:?}")))?;
    let mut post = Post::start(stream, host, path)?;
    post.write(&bytes)?;
    post.finish()?;
    decode_reply(&read_reply(stream)?)
}

/// Decodes a reply body as an IPP message.
pub fn decode_reply(body: &[u8]) -> io::Result<Message> {
    Message::decode(body)
        .map(|(message, _)| message)
        .map_err(|e| invalid(&std::format!("not an IPP reply ({e:?})")))
}
