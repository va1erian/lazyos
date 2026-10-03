//! Reading an ELF64 file header and its program headers from an [`Image`].
//!
//! Only what the loader needs is decoded, by hand, from little-endian bytes:
//! the identification, type and machine, the entry point and the program
//! header table. Everything is validated here; nothing is trusted later.

use alloc::vec::Vec;

use super::image::Image;

/// `PT_LOAD`: a segment the loader maps.
pub const PT_LOAD: u32 = 1;
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
/// a little-endian ELF64 x86-64 executable (`ET_EXEC`) or static PIE
/// (`ET_DYN`), version 1, with standard-size program headers inside the file.
pub fn read<I: Image + ?Sized>(image: &I) -> Result<Headers, &'static str> {
    let mut ehdr = [0u8; EHDR_SIZE];
    image
        .read_exact_at(0, &mut ehdr)
        .map_err(|_| "not a valid ELF")?;
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
        .map_err(|_| "out of memory")?;
    table.resize(table_len, 0);
    image
        .read_exact_at(phoff, &mut table)
        .map_err(|_| "program headers out of file")?;
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
        entry,
        phoff,
        phdrs,
    })
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
