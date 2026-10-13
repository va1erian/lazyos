//! Reading an ELF64 file header and its program headers from an [`Image`].
//!
//! Only what the loader needs is decoded, by hand, from little-endian bytes:
//! the identification, type and machine, the entry point and the program
//! header table. Everything is validated here; nothing is trusted later.

use alloc::vec::Vec;

use super::image::{Image, READ_FAILED};

/// `PT_LOAD`: a segment the loader maps.
pub const PT_LOAD: u32 = 1;
/// `PT_INTERP`: the path of the program interpreter (the dynamic linker).
pub const PT_INTERP: u32 = 3;
/// `e_type`: a fixed-address executable, and a position-independent one.
pub const ET_EXEC: u16 = 2;
pub const ET_DYN: u16 = 3;
/// Longest interpreter path accepted (`PATH_MAX`, NUL included).
pub const INTERP_MAX: u64 = 4096;
/// Segment permission bits (`p_flags`).
pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;

/// Size of the ELF64 file header.
const EHDR_SIZE: usize = 64;
/// Size of one ELF64 program header; the only `e_phentsize` accepted.
pub const PHDR_SIZE: u16 = 56;
/// `e_phnum` value meaning "the count is in section 0" (extended numbering);
/// never produced for executables, refused.
const PN_XNUM: u16 = 0xffff;

/// One program header, as stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Phdr {
    pub kind: u32,
    pub flags: u32,
    pub offset: u64,
    pub vaddr: u64,
    pub filesz: u64,
    pub memsz: u64,
}

/// The decoded headers of an executable.
#[derive(Debug)]
pub struct Headers {
    /// `e_type`: [`ET_EXEC`] or [`ET_DYN`].
    pub etype: u16,
    pub entry: u64,
    /// File offset of the program header table (for `AT_PHDR`).
    pub phoff: u64,
    pub phdrs: Vec<Phdr>,
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut word = [0u8; 8];
    word.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(word)
}

/// Read and validate the file header and every program header of `image`:
/// A header read that failed: an I/O error stays one ([`READ_FAILED`], so the
/// caller reports `EIO`); a short file is the image's fault (`invalid`).
fn io_or(error: &'static str, invalid: &'static str) -> &'static str {
    if error == READ_FAILED {
        error
    } else {
        invalid
    }
}

/// a little-endian ELF64 x86-64 executable (`ET_EXEC`) or static PIE
/// (`ET_DYN`), version 1, with standard-size program headers inside the file.
pub fn read<I: Image + ?Sized>(image: &I) -> Result<Headers, &'static str> {
    let mut ehdr = [0u8; EHDR_SIZE];
    image
        .read_exact_at(0, &mut ehdr)
        .map_err(|e| io_or(e, "not a valid ELF"))?;
    if ehdr[..4] != [0x7f, b'E', b'L', b'F'] || ehdr[4] != 2 || ehdr[5] != 1 || ehdr[6] != 1 {
        return Err("not a valid ELF");
    }
    if !matches!(u16_at(&ehdr, 16), 2 | 3) {
        return Err("not an executable ELF");
    }
    if u16_at(&ehdr, 18) != 0x3e {
        return Err("not an x86-64 ELF");
    }
    let entry = u64_at(&ehdr, 24);
    let phoff = u64_at(&ehdr, 32);
    let (phentsize, phnum) = (u16_at(&ehdr, 54), u16_at(&ehdr, 56));
    if phnum == PN_XNUM || (phnum != 0 && phentsize != PHDR_SIZE) {
        return Err("unsupported program header table");
    }
    let table_len = usize::from(phnum) * usize::from(PHDR_SIZE);
    let mut table = Vec::new();
    table
        .try_reserve_exact(table_len)
        .map_err(|_| super::loader::OUT_OF_MEMORY)?;
    table.resize(table_len, 0);
    image
        .read_exact_at(phoff, &mut table)
        .map_err(|e| io_or(e, "program headers out of file"))?;
    let phdrs = table
        .chunks_exact(usize::from(PHDR_SIZE))
        .map(|raw| Phdr {
            kind: u32_at(raw, 0),
            flags: u32_at(raw, 4),
            offset: u64_at(raw, 8),
            vaddr: u64_at(raw, 16),
            filesz: u64_at(raw, 32),
            memsz: u64_at(raw, 40),
        })
        .collect();
    Ok(Headers {
        etype: u16_at(&ehdr, 16),
        entry,
        phoff,
        phdrs,
    })
}

/// The program interpreter the image names with `PT_INTERP`, if any: the
/// NUL-terminated path stored in the file. A path that is empty, has no NUL
/// inside the segment, is longer than [`INTERP_MAX`] or is not UTF-8 makes
/// the image unloadable; several `PT_INTERP` headers are refused as Linux
/// does.
pub fn interpreter<I: Image + ?Sized>(
    image: &I,
    headers: &Headers,
) -> Result<Option<alloc::string::String>, &'static str> {
    let mut found = headers.phdrs.iter().filter(|ph| ph.kind == PT_INTERP);
    let Some(header) = found.next() else {
        return Ok(None);
    };
    if found.next().is_some() {
        return Err("more than one PT_INTERP");
    }
    if header.filesz < 2 || header.filesz > INTERP_MAX {
        return Err("bad interpreter path length");
    }
    header
        .offset
        .checked_add(header.filesz)
        .filter(|&end| end <= image.len())
        .ok_or("interpreter path out of file")?;
    let mut raw = alloc::vec![0u8; header.filesz as usize];
    image
        .read_exact_at(header.offset, &mut raw)
        .map_err(|e| io_or(e, "interpreter path out of file"))?;
    // The path ends at the first NUL, which the segment must contain.
    let end = raw
        .iter()
        .position(|&byte| byte == 0)
        .ok_or("interpreter path is not terminated")?;
    if end == 0 {
        return Err("empty interpreter path");
    }
    core::str::from_utf8(&raw[..end])
        .map(|path| Some(alloc::string::String::from(path)))
        .map_err(|_| "interpreter path is not UTF-8")
}

/// The runtime address of the program header table: the `PT_LOAD` segment
/// whose file bytes contain it, or 0 (musl then finds no `AT_PHDR`, which
/// only matters for TLS in a static image without one).
///
/// Only segments the loader would map are considered (`memsz != 0`,
/// `filesz <= memsz`): the loader skips an empty one before validating it,
/// so its untrusted `vaddr` must not reach this sum unchecked.
pub fn phdr_address(headers: &Headers) -> u64 {
    headers
        .phdrs
        .iter()
        .filter(|ph| ph.kind == PT_LOAD && ph.memsz != 0 && ph.filesz <= ph.memsz)
        .find(|ph| headers.phoff >= ph.offset && headers.phoff - ph.offset < ph.filesz)
        .and_then(|ph| ph.vaddr.checked_add(headers.phoff - ph.offset))
        .unwrap_or(0)
}
