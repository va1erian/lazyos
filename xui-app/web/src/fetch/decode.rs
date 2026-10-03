//! `Content-Encoding` decoding (`gzip`, `deflate`), streamed.
//!
//! NetSurf expects bodies as the server meant them, so the fetcher asks for
//! compressed responses (smaller over a slow emulated link) and inflates them
//! here, chunk by chunk, before NetSurf sees a byte.

use std::io::{self, BufRead, BufReader, Read};

use flate2::bufread::{DeflateDecoder, MultiGzDecoder, ZlibDecoder};

/// What a `Content-Encoding` value asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Coding {
    Identity,
    Gzip,
    Deflate,
    /// Something we never asked for (`br`, a chain of codings): passed on
    /// untouched, header included.
    Other,
}

impl Coding {
    pub(crate) fn of(header: Option<&str>) -> Coding {
        let Some(value) = header else {
            return Coding::Identity;
        };
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "identity" => Coding::Identity,
            "gzip" | "x-gzip" => Coding::Gzip,
            "deflate" => Coding::Deflate,
            _ => Coding::Other,
        }
    }

    /// Whether the fetcher decodes it (and so drops the header).
    pub(crate) fn decoded(self) -> bool {
        matches!(self, Coding::Gzip | Coding::Deflate)
    }
}

/// `raw` read through the decoder for `coding`.
///
/// An empty body stays empty whatever the header says (servers label 204s
/// and empty error pages `gzip`). `deflate` is meant to be zlib-wrapped, but
/// some servers send a raw deflate stream; the first two bytes tell them
/// apart.
pub(crate) fn reader<'a, R: Read + 'a>(raw: R, coding: Coding) -> io::Result<Box<dyn Read + 'a>> {
    let mut buffered = BufReader::with_capacity(16 * 1024, raw);
    if !coding.decoded() {
        return Ok(Box::new(buffered));
    }
    let head = buffered.fill_buf()?;
    if head.is_empty() {
        return Ok(Box::new(buffered));
    }
    Ok(match coding {
        Coding::Gzip => Box::new(MultiGzDecoder::new(buffered)),
        _ if is_zlib(head) => Box::new(ZlibDecoder::new(buffered)),
        _ => Box::new(DeflateDecoder::new(buffered)),
    })
}

/// A zlib header: compression method 8 and a header checksum (RFC 1950).
fn is_zlib(head: &[u8]) -> bool {
    match head {
        [cmf, flg, ..] => cmf & 0x0f == 8 && (u16::from(*cmf) << 8 | u16::from(*flg)) % 31 == 0,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};
    use flate2::Compression;

    use super::*;

    fn decode(bytes: &[u8], coding: Coding) -> Vec<u8> {
        let mut out = Vec::new();
        reader(bytes, coding)
            .unwrap()
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    #[test]
    fn header_values() {
        assert_eq!(Coding::of(None), Coding::Identity);
        assert_eq!(Coding::of(Some(" GZip ")), Coding::Gzip);
        assert_eq!(Coding::of(Some("x-gzip")), Coding::Gzip);
        assert_eq!(Coding::of(Some("deflate")), Coding::Deflate);
        assert_eq!(Coding::of(Some("br")), Coding::Other);
        assert_eq!(Coding::of(Some("gzip, br")), Coding::Other);
    }

    #[test]
    fn gzip_zlib_and_raw_deflate_decode() {
        let text = b"<html>hello hello hello</html>".repeat(50);
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&text).unwrap();
        assert_eq!(decode(&gz.finish().unwrap(), Coding::Gzip), text);
        let mut zlib = ZlibEncoder::new(Vec::new(), Compression::default());
        zlib.write_all(&text).unwrap();
        assert_eq!(decode(&zlib.finish().unwrap(), Coding::Deflate), text);
        let mut raw = DeflateEncoder::new(Vec::new(), Compression::default());
        raw.write_all(&text).unwrap();
        assert_eq!(decode(&raw.finish().unwrap(), Coding::Deflate), text);
    }

    #[test]
    fn empty_bodies_and_unknown_codings_pass_through() {
        assert!(decode(b"", Coding::Gzip).is_empty());
        assert_eq!(decode(b"abc", Coding::Other), b"abc");
    }

    #[test]
    fn corrupt_gzip_is_an_error() {
        let mut out = Vec::new();
        let result = reader(&b"\x1f\x8bnot really gzip"[..], Coding::Gzip)
            .unwrap()
            .read_to_end(&mut out);
        assert!(result.is_err());
    }
}
