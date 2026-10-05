//! The HTTP side: framing, chunking and the reply reader.

use std::io::{self, Cursor, Read, Write};

use super::*;
use crate::http::{exchange, read_reply, Post, MAX_BODY};

/// A connection that replies with canned bytes and records what was sent.
struct Fake {
    reply: Cursor<Vec<u8>>,
    sent: Vec<u8>,
}

impl Read for Fake {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Small reads, to exercise every buffering path.
        let n = buf.len().min(7);
        self.reply.read(&mut buf[..n])
    }
}

impl Write for Fake {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.sent.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn fake(reply: &[u8]) -> Fake {
    Fake {
        reply: Cursor::new(reply.to_vec()),
        sent: Vec::new(),
    }
}

#[test]
fn a_post_is_chunked() {
    let mut out = Vec::new();
    let mut post = Post::start(&mut out, "10.0.2.2:631", "/ipp/print").unwrap();
    post.write(b"hello").unwrap();
    post.write(b"").unwrap();
    post.write(&[b'x'; 300]).unwrap();
    post.finish().unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.starts_with("POST /ipp/print HTTP/1.1\r\nHost: 10.0.2.2:631\r\n"));
    assert!(text.contains("Content-Type: application/ipp\r\n"));
    assert!(text.contains("\r\n\r\n5\r\nhello\r\n12c\r\nxxx"));
    assert!(text.ends_with("x\r\n0\r\n\r\n"));
    assert!(Post::start(&mut Vec::new(), "a\r\nb", "/").is_err());
}

#[test]
fn replies_are_read_by_length_chunks_or_close() {
    let body = b"\x02\x00\x00\x00\x00\x00\x00\x01\x03";
    let mut by_length =
        b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\n".to_vec();
    by_length.extend_from_slice(body);
    by_length.extend_from_slice(b"trailing junk");
    assert_eq!(read_reply(&mut fake(&by_length)).unwrap(), body);

    let mut chunked = b"HTTP/1.1 200 OK\r\ntransfer-encoding: Chunked\r\n\r\n4\r\n".to_vec();
    chunked.extend_from_slice(&body[..4]);
    chunked.extend_from_slice(b"\r\n5;ext=1\r\n");
    chunked.extend_from_slice(&body[4..]);
    chunked.extend_from_slice(b"\r\n0\r\n\r\n");
    assert_eq!(read_reply(&mut fake(&chunked)).unwrap(), body);

    let mut closed = b"HTTP/1.0 200 OK\r\n\r\n".to_vec();
    closed.extend_from_slice(body);
    assert_eq!(read_reply(&mut fake(&closed)).unwrap(), body);
}

#[test]
fn bad_replies_are_errors() {
    for reply in [
        &b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n"[..],
        b"SSH-2.0-OpenSSH\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: x\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nabX\r\n",
        // A chunk size whose sum with the body so far would overflow.
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\na\r\nffffffffffffffff\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort",
        b"HTTP/1.1 200",
    ] {
        assert!(read_reply(&mut fake(reply)).is_err(), "{reply:?}");
    }
    let huge = std::format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
        MAX_BODY + 1
    );
    assert!(read_reply(&mut fake(huge.as_bytes())).is_err());
    let mut endless = b"HTTP/1.1 200 OK\r\n\r\n".to_vec();
    endless.resize(endless.len() + MAX_BODY + 10, b'a');
    assert!(read_reply(&mut fake(&endless)).is_err());
}

#[test]
fn an_exchange_sends_the_request_and_decodes_the_reply() {
    let reply = deskjet_reply().encode().unwrap();
    let mut wire =
        std::format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", reply.len()).into_bytes();
    wire.extend_from_slice(&reply);
    let mut conn = fake(&wire);
    let request = request::get_printer_attributes(&CLIENT, 1, request::STATUS_ATTRIBUTES);
    let got = exchange(&mut conn, "192.168.1.89:631", "/ipp/print", &request).unwrap();
    assert_eq!(got, deskjet_reply());
    let sent = conn.sent;
    let body_at = sent.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    let size_end = body_at
        + sent[body_at..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .unwrap();
    let size =
        usize::from_str_radix(std::str::from_utf8(&sent[body_at..size_end]).unwrap(), 16).unwrap();
    let (sent_request, _) = Message::decode(&sent[size_end + 2..size_end + 2 + size]).unwrap();
    assert_eq!(sent_request, request);
}
