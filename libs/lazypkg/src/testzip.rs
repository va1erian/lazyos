//! A tiny in-test zip writer.
//!
//! Tests build archives in memory (stored and deflated entries) so the crate
//! needs no binary fixtures. The writer is intentionally dumb: it writes what a
//! [`Member`] says, including fields that are deliberately wrong, so the reader
//! can be driven down every rejection path.

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use crate::inflate::crc32;
use crate::zip::{CENTRAL_SIG, EOCD_SIG, LOCAL_SIG};

/// The 8-byte PNG signature, followed by a few bytes so the entry has content.
pub(crate) fn png() -> Vec<u8> {
    let mut bytes = crate::PNG_SIG.to_vec();
    bytes.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
    bytes
}

/// One archive member, with optional overrides that make it malformed.
pub(crate) struct Member<'a> {
    pub name: &'a str,
    pub data: Vec<u8>,
    pub deflate: bool,
    /// Method written to both headers; defaults to 8/0 from `deflate`.
    pub method: Option<u16>,
    /// Method written only to the local header, to desync it from the central.
    pub local_method: Option<u16>,
    /// General purpose flags written to both headers.
    pub flags: u16,
    /// Declared uncompressed size; defaults to `data.len()`.
    pub declared_size: Option<u32>,
    /// Declared compressed size; defaults to the compressed length.
    pub declared_compressed: Option<u32>,
    /// Extra field written to both headers.
    pub extra: Vec<u8>,
    /// CRC-32 written to both headers; defaults to the real one.
    pub crc: Option<u32>,
}

impl<'a> Member<'a> {
    pub(crate) fn stored(name: &'a str, data: Vec<u8>) -> Member<'a> {
        Member {
            name,
            data,
            deflate: false,
            method: None,
            local_method: None,
            flags: 0,
            declared_size: None,
            declared_compressed: None,
            extra: Vec::new(),
            crc: None,
        }
    }

    pub(crate) fn deflated(name: &'a str, data: Vec<u8>) -> Member<'a> {
        Member {
            deflate: true,
            ..Member::stored(name, data)
        }
    }

    pub(crate) fn declared_size(mut self, size: u32) -> Member<'a> {
        self.declared_size = Some(size);
        self
    }
}

/// The minimal valid manifest text referencing `binary`.
pub(crate) fn manifest(binary: &str) -> Vec<u8> {
    format!(
        "[app]\nname = \"Demo\"\nsystem_name = \"org.lazy.demo\"\nauthor = \"Tester\"\nversion = \"1.0.0\"\n\n[entry]\nbinary = \"{binary}\"\n"
    )
    .into_bytes()
}

/// The five members of a minimal valid package.
pub(crate) fn valid_members(deflate: bool) -> Vec<Member<'static>> {
    let mut members = vec![
        Member::stored("manifest.toml", manifest("bin/app.elf")),
        Member::stored("bin/app.elf", b"ELF fake binary".to_vec()),
        Member::stored("icons/app-16.png", png()),
        Member::stored("icons/app-32.png", png()),
        Member::stored("icons/app-128.png", png()),
    ];
    if deflate {
        for member in &mut members {
            member.deflate = true;
        }
    }
    members
}

/// A minimal valid package, all stored or all deflated.
pub(crate) fn valid(deflate: bool) -> Vec<u8> {
    build(&valid_members(deflate))
}

/// Serialize `members` into a zip archive.
pub(crate) fn build(members: &[Member<'_>]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for member in members {
        let offset = out.len() as u32;
        let crc = member.crc.unwrap_or_else(|| crc32(&member.data));
        let compressed = if member.deflate {
            miniz_oxide::deflate::compress_to_vec(&member.data, 6)
        } else {
            member.data.clone()
        };
        let method = member.method.unwrap_or(if member.deflate { 8 } else { 0 });
        let local_method = member.local_method.unwrap_or(method);
        let size = member.declared_size.unwrap_or(member.data.len() as u32);
        let compressed_size = member
            .declared_compressed
            .unwrap_or(compressed.len() as u32);

        push_u32(&mut out, LOCAL_SIG);
        push_u16(&mut out, 20);
        push_u16(&mut out, member.flags);
        push_u16(&mut out, local_method);
        push_u16(&mut out, 0);
        push_u16(&mut out, 0);
        push_u32(&mut out, crc);
        push_u32(&mut out, compressed_size);
        push_u32(&mut out, size);
        push_u16(&mut out, member.name.len() as u16);
        push_u16(&mut out, member.extra.len() as u16);
        out.extend_from_slice(member.name.as_bytes());
        out.extend_from_slice(&member.extra);
        out.extend_from_slice(&compressed);

        push_u32(&mut central, CENTRAL_SIG);
        push_u16(&mut central, 20);
        push_u16(&mut central, 20);
        push_u16(&mut central, member.flags);
        push_u16(&mut central, method);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u32(&mut central, crc);
        push_u32(&mut central, compressed_size);
        push_u32(&mut central, size);
        push_u16(&mut central, member.name.len() as u16);
        push_u16(&mut central, member.extra.len() as u16);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u32(&mut central, 0);
        push_u32(&mut central, offset);
        central.extend_from_slice(member.name.as_bytes());
        central.extend_from_slice(&member.extra);
    }
    let central_offset = out.len() as u32;
    let central_size = central.len() as u32;
    out.extend_from_slice(&central);

    push_u32(&mut out, EOCD_SIG);
    push_u16(&mut out, 0);
    push_u16(&mut out, 0);
    push_u16(&mut out, members.len() as u16);
    push_u16(&mut out, members.len() as u16);
    push_u32(&mut out, central_size);
    push_u32(&mut out, central_offset);
    push_u16(&mut out, 0);
    out
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}
