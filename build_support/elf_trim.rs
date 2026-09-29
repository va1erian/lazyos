//! Trim an ELF64 image down to what the bootloader needs (boot-time work).
//!
//! The bootloader reads the whole kernel file through BIOS `int 13h`
//! before it maps a single segment. A dev-profile kernel carries megabytes of
//! DWARF and symbols after its loadable data (5.1 MB file, 1.4 MB loaded), and
//! every byte of it is a slow emulated disk read. Dropping them cuts boot by
//! roughly half a second under WHPX, while `target/` keeps the full ELF for
//! panic triage.
//!
//! The bootloader finds its `.bootloader-config` (physical-memory mapping,
//! logging) **by section name**, so the trimmed file keeps a section table:
//! the null entry, every `SHF_ALLOC` section (their data lies inside the
//! `PT_LOAD` prefix we keep), and a fresh `.shstrtab` naming them. Anything
//! unexpected returns `None` and the caller ships the original bytes: a
//! surprise degrades to "slower", never "broken".

const EHDR_SIZE: usize = 64;
const SHDR_SIZE: usize = 64;
const PT_LOAD: u32 = 1;
const SHF_ALLOC: u64 = 2;
const SHT_NOBITS: u32 = 8;
const SHT_STRTAB: u32 = 3;

fn u16_at(b: &[u8], off: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(off..off + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], off: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(off..off + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], off: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(off..off + 8)?.try_into().ok()?))
}

/// A NUL-terminated name in a string table.
fn name_at(strtab: &[u8], off: usize) -> Option<&[u8]> {
    let rest = strtab.get(off..)?;
    Some(&rest[..rest.iter().position(|&c| c == 0)?])
}

/// End of the file bytes the program headers reference (and the headers).
fn loadable_end(elf: &[u8]) -> Option<usize> {
    let phoff = u64_at(elf, 0x20)? as usize;
    let phentsize = u16_at(elf, 0x36)? as usize;
    let phnum = u16_at(elf, 0x38)? as usize;
    if phentsize < 56 {
        return None;
    }
    let mut keep = EHDR_SIZE.max(phoff.checked_add(phnum.checked_mul(phentsize)?)?);
    for index in 0..phnum {
        let ph = phoff + index * phentsize;
        if u32_at(elf, ph)? != PT_LOAD {
            continue;
        }
        let end = u64_at(elf, ph + 8)?.checked_add(u64_at(elf, ph + 32)?)? as usize;
        keep = keep.max(end);
    }
    Some(keep)
}

/// Return `elf` cut after its loadable data, plus a rebuilt section table
/// holding only the `SHF_ALLOC` sections. `None` if `elf` is not a
/// little-endian ELF64 or anything points outside the file.
pub fn trim_to_loadable(elf: &[u8]) -> Option<Vec<u8>> {
    if elf.get(..4)? != b"\x7fELF" || *elf.get(4)? != 2 || *elf.get(5)? != 1 {
        return None;
    }
    let keep = loadable_end(elf)?;
    let shoff = u64_at(elf, 0x28)? as usize;
    let shentsize = u16_at(elf, 0x3A)? as usize;
    let shnum = u16_at(elf, 0x3C)? as usize;
    let shstrndx = u16_at(elf, 0x3E)? as usize;
    if keep > elf.len() || shentsize != SHDR_SIZE || shstrndx >= shnum {
        return None;
    }
    let old_shdr = |i: usize| elf.get(shoff + i * SHDR_SIZE..shoff + (i + 1) * SHDR_SIZE);
    let strtab_hdr = old_shdr(shstrndx)?;
    let (str_off, str_len) = (
        u64_at(strtab_hdr, 24)? as usize,
        u64_at(strtab_hdr, 32)? as usize,
    );
    let strtab = elf.get(str_off..str_off.checked_add(str_len)?)?;

    // New string table: leading NUL, then each kept section's name.
    let mut names = vec![0u8];
    let mut headers: Vec<[u8; SHDR_SIZE]> = vec![[0; SHDR_SIZE]];
    for index in 1..shnum {
        let old = old_shdr(index)?;
        if u64_at(old, 8)? & SHF_ALLOC == 0 || index == shstrndx {
            continue;
        }
        let (kind, off, size) = (u32_at(old, 4)?, u64_at(old, 24)?, u64_at(old, 32)?);
        if kind != SHT_NOBITS && off.checked_add(size)? as usize > keep {
            return None; // allocated data beyond the loadable prefix
        }
        let name = name_at(strtab, u32_at(old, 0)? as usize)?;
        let mut header = [0u8; SHDR_SIZE];
        header.copy_from_slice(old);
        header[0..4].copy_from_slice(&(names.len() as u32).to_le_bytes());
        // Links/infos index the old table; the kept sections use none.
        header[40..48].fill(0);
        names.extend_from_slice(name);
        names.push(0);
        headers.push(header);
    }
    let self_name = names.len() as u32;
    names.extend_from_slice(b".shstrtab\0");

    let mut out = elf[..keep].to_vec();
    let names_off = out.len();
    out.extend_from_slice(&names);
    out.resize(out.len().next_multiple_of(8), 0);
    let new_shoff = out.len();
    let mut table = [0u8; SHDR_SIZE];
    table[0..4].copy_from_slice(&self_name.to_le_bytes());
    table[4..8].copy_from_slice(&SHT_STRTAB.to_le_bytes());
    table[24..32].copy_from_slice(&(names_off as u64).to_le_bytes());
    table[32..40].copy_from_slice(&(names.len() as u64).to_le_bytes());
    table[48..56].copy_from_slice(&1u64.to_le_bytes());
    headers.push(table);
    for header in &headers {
        out.extend_from_slice(header);
    }
    out[0x28..0x30].copy_from_slice(&(new_shoff as u64).to_le_bytes());
    out[0x3C..0x3E].copy_from_slice(&(headers.len() as u16).to_le_bytes());
    out[0x3E..0x40].copy_from_slice(&((headers.len() - 1) as u16).to_le_bytes());
    Some(out)
}
