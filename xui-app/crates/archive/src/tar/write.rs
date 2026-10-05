//! The tar writer: ustar headers, with a pax header in front of a member
//! whose name, link target, size or time does not fit ustar's fields.

use std::io::{Read, Write};

use crate::error::Result;
use crate::writer::{copy_exact, EntryWriter, FinishOut, Meta};

use super::{padding, BLOCK};

/// ustar's largest size and time (11 octal digits).
const OCTAL_11_MAX: u64 = 0o77_777_777_777;

/// Writes a tarball into a (possibly compressing) stream.
pub struct TarWriter {
    out: FinishOut,
}

impl TarWriter {
    pub fn new(out: FinishOut) -> TarWriter {
        TarWriter { out }
    }

    /// Write one member's header (and the pax header it needs first).
    fn header(&mut self, path: &str, flag: u8, size: u64, meta: Meta, link: &str) -> Result<()> {
        let mut pax = Vec::new();
        let (name, prefix) = match split_ustar(path) {
            Some(parts) => parts,
            None => {
                pax_record(&mut pax, "path", path);
                (truncate(path, 100), String::new())
            }
        };
        if link.len() > 100 {
            pax_record(&mut pax, "linkpath", link);
        }
        if size > OCTAL_11_MAX {
            pax_record(&mut pax, "size", &size.to_string());
        }
        let mtime = meta.modified.unwrap_or(0);
        let mtime_fits = (0..=OCTAL_11_MAX as i64).contains(&mtime);
        if !mtime_fits {
            pax_record(&mut pax, "mtime", &mtime.to_string());
        }
        if !pax.is_empty() {
            let pax_name = format!("PaxHeaders/{}", truncate(&name, 80));
            let block = build_header(&pax_name, "", b'x', pax.len() as u64, 0o644, 0, "");
            self.out.write_all(&block)?;
            self.out.write_all(&pax)?;
            self.out
                .write_all(&vec![0u8; padding(pax.len() as u64) as usize])?;
        }
        let mode = meta
            .mode
            .unwrap_or(if flag == b'5' { 0o755 } else { 0o644 })
            & 0o7777;
        let block = build_header(
            &name,
            &prefix,
            flag,
            size.min(OCTAL_11_MAX),
            mode,
            if mtime_fits { mtime as u64 } else { 0 },
            &truncate(link, 100),
        );
        self.out.write_all(&block)?;
        Ok(())
    }
}

impl EntryWriter for TarWriter {
    fn dir(&mut self, path: &str, meta: Meta) -> Result<()> {
        self.header(&format!("{path}/"), b'5', 0, meta, "")
    }

    fn file(&mut self, path: &str, meta: Meta, size: u64, data: &mut dyn Read) -> Result<()> {
        self.header(path, b'0', size, meta, "")?;
        copy_exact(data, &mut self.out, size)?;
        self.out.write_all(&vec![0u8; padding(size) as usize])?;
        Ok(())
    }

    fn symlink(&mut self, path: &str, meta: Meta, target: &str) -> Result<()> {
        let meta = Meta {
            mode: Some(meta.mode.unwrap_or(0o777)),
            ..meta
        };
        self.header(path, b'2', 0, meta, target)
    }

    fn hardlink(&mut self, path: &str, meta: Meta, target: &str) -> Result<()> {
        self.header(path, b'1', 0, meta, target)
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        self.out.write_all(&[0u8; BLOCK * 2])?;
        self.out.finish()?;
        Ok(())
    }
}

/// `path` as ustar's `(name, prefix)` pair, when it fits: a name of at most
/// 100 bytes, split at a `/` with at most 155 bytes before it.
fn split_ustar(path: &str) -> Option<(String, String)> {
    if path.len() <= 100 {
        return Some((path.to_owned(), String::new()));
    }
    let bytes = path.as_bytes();
    (0..bytes.len())
        .rev()
        .filter(|&i| bytes[i] == b'/')
        .find(|&i| i <= 155 && bytes.len() - i - 1 <= 100 && i > 0)
        .map(|i| (path[i + 1..].to_owned(), path[..i].to_owned()))
}

/// `text` cut to at most `max` bytes on a character boundary.
fn truncate(text: &str, max: usize) -> String {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// Append one pax record: its length counts its own digits.
fn pax_record(out: &mut Vec<u8>, key: &str, value: &str) {
    let body = key.len() + value.len() + 3; // ' ', '=', '\n'
    let mut len = body + 1;
    while len != body + len.to_string().len() {
        len = body + len.to_string().len();
    }
    out.extend_from_slice(format!("{len} {key}={value}\n").as_bytes());
}

/// One ustar header block.
pub(crate) fn build_header(
    name: &str,
    prefix: &str,
    flag: u8,
    size: u64,
    mode: u32,
    mtime: u64,
    link: &str,
) -> [u8; BLOCK] {
    let mut block = [0u8; BLOCK];
    put(&mut block[0..100], name.as_bytes());
    put_octal(&mut block[100..108], u64::from(mode));
    put_octal(&mut block[108..116], 0);
    put_octal(&mut block[116..124], 0);
    put_octal(&mut block[124..136], size);
    put_octal(&mut block[136..148], mtime);
    block[156] = flag;
    put(&mut block[157..257], link.as_bytes());
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    put(&mut block[345..500], prefix.as_bytes());
    block[148..156].fill(b' ');
    let sum: u64 = block.iter().map(|&b| u64::from(b)).sum();
    put(&mut block[148..156], format!("{sum:06o}\0 ").as_bytes());
    block
}

fn put(field: &mut [u8], value: &[u8]) {
    let n = value.len().min(field.len());
    field[..n].copy_from_slice(&value[..n]);
}

/// A zero-padded octal number filling the field but its final NUL.
fn put_octal(field: &mut [u8], value: u64) {
    let width = field.len() - 1;
    put(field, format!("{value:0width$o}").as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pax_lengths_count_themselves() {
        for value in ["x", &"y".repeat(90), &"z".repeat(995)] {
            let mut out = Vec::new();
            pax_record(&mut out, "path", value);
            let text = String::from_utf8(out.clone()).unwrap();
            let len: usize = text.split(' ').next().unwrap().parse().unwrap();
            assert_eq!(len, out.len());
        }
    }

    #[test]
    fn long_paths_split_at_a_slash() {
        let path = format!("{}/{}", "d".repeat(120), "f".repeat(90));
        let (name, prefix) = split_ustar(&path).unwrap();
        assert_eq!(format!("{prefix}/{name}"), path);
        assert!(split_ustar(&"n".repeat(300)).is_none());
    }

    #[test]
    fn headers_pass_their_own_checksum() {
        let block = build_header("a.txt", "", b'0', 5, 0o644, 1, "");
        assert!(super::super::is_header(&block));
    }
}
