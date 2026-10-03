//! The response body: gzip decoding and the size cap.
//!
//! The cap applies twice: to the bytes on the wire and to the decoded bytes.
//! The decoded cap stops a gzip bomb (a few KiB that inflate to gigabytes);
//! the wire cap stops a stream that never ends (gzip can carry endless empty
//! blocks that decode to nothing).

use std::io::{self, Read, Write};

use flate2::read::MultiGzDecoder;

use crate::report::Failure;

/// The most body bytes a run accepts (docs/tls-plan.md §7).
pub const BODY_CAP: u64 = 64 * 1024 * 1024;

/// How the body is encoded on the wire, from `Content-Encoding`.
#[derive(Debug, PartialEq, Eq)]
pub enum Encoding {
    Identity,
    Gzip,
    /// Anything else is passed through undecoded, as curl does without
    /// `--compressed`.
    Other,
}

impl Encoding {
    /// Classify a `Content-Encoding` value (absent means identity).
    pub fn from_header(value: Option<&[u8]>) -> Encoding {
        let Some(raw) = value else {
            return Encoding::Identity;
        };
        let text = String::from_utf8_lossy(raw).trim().to_ascii_lowercase();
        match text.as_str() {
            "" | "identity" => Encoding::Identity,
            "gzip" | "x-gzip" => Encoding::Gzip,
            _ => Encoding::Other,
        }
    }
}

/// A reader that fails once more than `cap` bytes came through it.
struct Capped<R> {
    inner: R,
    remaining: u64,
}

/// The error a [`Capped`] reader raises, recognised by [`copy_body`].
#[derive(Debug)]
struct CapExceeded;

impl std::fmt::Display for CapExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("body cap exceeded")
    }
}

impl std::error::Error for CapExceeded {}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n as u64 > self.remaining {
            return Err(io::Error::other(CapExceeded));
        }
        self.remaining -= n as u64;
        Ok(n)
    }
}

/// Decode (when gzip) and copy `raw` to `out`, at most `cap` bytes each way.
/// Returns the number of decoded bytes written.
pub fn copy_body(
    raw: impl Read,
    encoding: &Encoding,
    out: &mut dyn Write,
    cap: u64,
) -> Result<u64, Failure> {
    let wire = Capped {
        inner: raw,
        remaining: cap,
    };
    let decoded: Box<dyn Read> = match encoding {
        Encoding::Gzip => Box::new(MultiGzDecoder::new(wire)),
        _ => Box::new(wire),
    };
    let mut body = Capped {
        inner: decoded,
        remaining: cap,
    };
    let mut buf = vec![0u8; 32 * 1024];
    let mut total = 0u64;
    loop {
        let n = match body.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(read_failure(e, cap, encoding)),
        };
        out.write_all(&buf[..n])
            .map_err(|e| Failure::Write(e.to_string()))?;
        total += n as u64;
    }
    out.flush().map_err(|e| Failure::Write(e.to_string()))?;
    Ok(total)
}

/// Map a read error to the failure a person should see.
fn read_failure(error: io::Error, cap: u64, encoding: &Encoding) -> Failure {
    if is_cap(&error) {
        return Failure::BodyTooLarge(cap);
    }
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Failure::Timeout,
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData if *encoding == Encoding::Gzip => {
            Failure::Recv(format!("bad gzip data: {error}"))
        }
        _ => Failure::Recv(crate::sanitize::printable_str(&error.to_string())),
    }
}

/// Whether `error` (possibly wrapped by the gzip decoder) is the cap.
fn is_cap(error: &io::Error) -> bool {
    let mut current: Option<&(dyn std::error::Error + 'static)> = error.get_ref().map(|e| e as _);
    while let Some(e) = current {
        if e.is::<CapExceeded>() {
            return true;
        }
        if let Some(io) = e.downcast_ref::<io::Error>() {
            current = io.get_ref().map(|e| e as _);
        } else {
            current = e.source();
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;

    fn gzip(data: &[u8]) -> Vec<u8> {
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn encodings() {
        assert_eq!(Encoding::from_header(None), Encoding::Identity);
        assert_eq!(Encoding::from_header(Some(b" GZIP ")), Encoding::Gzip);
        assert_eq!(Encoding::from_header(Some(b"x-gzip")), Encoding::Gzip);
        assert_eq!(Encoding::from_header(Some(b"br")), Encoding::Other);
    }

    #[test]
    fn identity_passes_through() {
        let mut out = Vec::new();
        let n = copy_body(&b"hello"[..], &Encoding::Identity, &mut out, 100).unwrap();
        assert_eq!((n, out.as_slice()), (5, &b"hello"[..]));
    }

    #[test]
    fn gzip_is_decoded() {
        let page = b"<html>".repeat(1000);
        let mut out = Vec::new();
        let n = copy_body(gzip(&page).as_slice(), &Encoding::Gzip, &mut out, BODY_CAP).unwrap();
        assert_eq!(n as usize, page.len());
        assert_eq!(out, page);
    }

    #[test]
    fn cap_on_plain_body() {
        let mut out = Vec::new();
        let err = copy_body(&[0u8; 101][..], &Encoding::Identity, &mut out, 100).unwrap_err();
        assert_eq!(err, Failure::BodyTooLarge(100));
        let mut out = Vec::new();
        assert_eq!(
            copy_body(&[0u8; 100][..], &Encoding::Identity, &mut out, 100).unwrap(),
            100
        );
    }

    #[test]
    fn cap_on_gzip_bomb() {
        // 1 MiB of zeros compresses to about a kilobyte; the decoded cap fires.
        let bomb = gzip(&vec![0u8; 1024 * 1024]);
        assert!(bomb.len() < 4096);
        let mut out = Vec::new();
        let err = copy_body(bomb.as_slice(), &Encoding::Gzip, &mut out, 64 * 1024).unwrap_err();
        assert_eq!(err, Failure::BodyTooLarge(64 * 1024));
    }

    #[test]
    fn cap_on_compressed_wire_bytes() {
        // Incompressible data: the wire cap fires even though decoding works.
        let noise: Vec<u8> = (0..200_000u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let packed = gzip(&noise);
        let mut out = Vec::new();
        let err = copy_body(
            packed.as_slice(),
            &Encoding::Gzip,
            &mut out,
            (packed.len() / 2) as u64,
        );
        assert!(matches!(err, Err(Failure::BodyTooLarge(_))));
    }

    #[test]
    fn corrupt_gzip_is_an_error() {
        let mut out = Vec::new();
        let err = copy_body(
            &b"\x1f\x8bnot gzip at all"[..],
            &Encoding::Gzip,
            &mut out,
            100,
        )
        .unwrap_err();
        assert!(matches!(err, Failure::Recv(_)), "{err:?}");
    }
}
