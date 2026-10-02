//! Loading a static Linux ELF64 image and building its initial process start
//! stack (`argc`/`argv`/`envp`/`auxv`), shared by the initial [`load`] and by
//! `execve` (`procctl::sys_execve`), which replaces a running image with a
//! fresh one the same way.

use alloc::vec::Vec;

use x86_64::PhysAddr;
use xmas_elf::program::Type as ProgramType;
use xmas_elf::ElfFile;

use crate::mem::vma::{Kind, Prot};
use crate::process::{load_segments, map_range_kind, page_phys};

use super::uaccess::fill_random;
use super::{BRK_BASE, MMAP_LIMIT, PAGE, STACK_SIZE, STACK_TOP};

/// Windows an image may not occupy: the stack, `brk` and `mmap` regions.
pub(super) const LOAD_RESERVED: [(u64, u64); 1] = [(BRK_BASE, MMAP_LIMIT)];

/// Load a Linux image into `table`, build its start stack with `argv` and
/// `envp` (each item as given, without a NUL; one is added), and return
/// `(entry, stack_pointer)`.
pub fn load(
    table: PhysAddr,
    elf_bytes: &[u8],
    argv: &[&[u8]],
    envp: &[&[u8]],
) -> Result<(u64, u64), &'static str> {
    let entry = load_segments(table, elf_bytes, &LOAD_RESERVED)?;
    let stack = map_range_kind(
        table,
        STACK_TOP - STACK_SIZE,
        STACK_TOP,
        Prot::READ | Prot::WRITE,
        Kind::Stack,
    )?;

    let phdr = program_header_addr(elf_bytes);
    let (phent, phnum) = phdr_size(elf_bytes);
    let argv = nul_terminated(argv);
    let envp = nul_terminated(envp);
    let rsp = build_start_stack(
        &stack,
        &argv,
        &envp,
        entry,
        (phdr, phent, phnum),
        // A kernel-started program is root (see `task::spawn_linux`).
        (0, 0),
    );
    Ok((entry, rsp))
}

/// Each item followed by a NUL, as the start stack stores strings.
fn nul_terminated(items: &[&[u8]]) -> Vec<Vec<u8>> {
    items
        .iter()
        .map(|item| {
            let mut bytes = Vec::with_capacity(item.len() + 1);
            bytes.extend_from_slice(item);
            bytes.push(0);
            bytes
        })
        .collect()
}

/// Runtime address of the program headers (within a `PT_LOAD` segment).
pub(super) fn program_header_addr(elf_bytes: &[u8]) -> u64 {
    let Ok(elf) = ElfFile::new(elf_bytes) else {
        return 0;
    };
    let phoff = elf.header.pt2.ph_offset();
    for ph in elf.program_iter() {
        if ph.get_type() != Ok(ProgramType::Load) {
            continue;
        }
        let start = ph.offset();
        let end = start + ph.file_size();
        if phoff >= start && phoff < end {
            return ph.virtual_addr() + (phoff - start);
        }
    }
    0
}

pub(super) fn phdr_size(elf_bytes: &[u8]) -> (u16, u16) {
    match ElfFile::new(elf_bytes) {
        Ok(elf) => (elf.header.pt2.ph_entry_size(), elf.header.pt2.ph_count()),
        Err(_) => (0, 0),
    }
}

/// Build the Linux process start stack: `argc/argv/envp/auxv` plus strings.
/// `argv`/`envp` are NUL-terminated byte strings; `ids` is the `(uid, gid)`
/// reported through `AT_UID`/`AT_EUID`/`AT_GID`/`AT_EGID`.
pub(super) fn build_start_stack(
    stack: &[(u64, u64)],
    argv: &[Vec<u8>],
    envp: &[Vec<u8>],
    entry: u64,
    (phdr, phent, phnum): (u64, u16, u16),
    (uid, gid): (u32, u32),
) -> u64 {
    let mut cursor = STACK_TOP;

    // Helper: write bytes just below `cursor`.
    let push_bytes = |bytes: &[u8], cursor: &mut u64| -> u64 {
        *cursor -= bytes.len() as u64;
        write_user(stack, *cursor, bytes);
        *cursor
    };

    // Strings (any order; the arrays below hold their addresses).
    let mut random = [0u8; 16];
    fill_random(&mut random);
    let random_addr = push_bytes(&random, &mut cursor);
    let execfn = argv
        .first()
        .map(|a| push_bytes(a, &mut cursor))
        .unwrap_or(0);
    let argv_ptrs: Vec<u64> = argv.iter().map(|a| push_bytes(a, &mut cursor)).collect();
    let envp_ptrs: Vec<u64> = envp.iter().map(|e| push_bytes(e, &mut cursor)).collect();

    // Word arrays (low to high): argc, argv[], NULL, envp[], NULL, auxv, AT_NULL.
    let mut words: Vec<u64> = Vec::new();
    words.push(argv.len() as u64);
    words.extend_from_slice(&argv_ptrs);
    words.push(0); // argv NULL
    words.extend_from_slice(&envp_ptrs);
    words.push(0); // envp NULL
    let auxv: [(u64, u64); 13] = [
        (AT_PHDR, phdr),
        (AT_PHENT, phent as u64),
        (AT_PHNUM, phnum as u64),
        (AT_PAGESZ, PAGE),
        (AT_BASE, 0),
        (AT_ENTRY, entry),
        (AT_UID, uid as u64),
        (AT_EUID, uid as u64),
        (AT_GID, gid as u64),
        (AT_EGID, gid as u64),
        (AT_CLKTCK, 100),
        (AT_RANDOM, random_addr),
        (AT_EXECFN, execfn),
    ];
    for (kind, value) in auxv {
        words.push(kind);
        words.push(value);
    }
    words.push(0); // AT_NULL
    words.push(0);

    cursor -= (words.len() as u64) * 8;
    cursor &= !0xF; // 16-byte aligned stack
    for (i, word) in words.iter().enumerate() {
        write_user(stack, cursor + (i as u64) * 8, &word.to_le_bytes());
    }
    cursor
}

const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_BASE: u64 = 7;
const AT_ENTRY: u64 = 9;
const AT_UID: u64 = 11;
const AT_EUID: u64 = 12;
const AT_GID: u64 = 13;
const AT_EGID: u64 = 14;
const AT_CLKTCK: u64 = 17;
const AT_RANDOM: u64 = 25;
const AT_EXECFN: u64 = 31;

/// Write bytes into a mapped user page set (via the kernel's phys map),
/// page by page: the pages are virtually contiguous but their frames are
/// not, so a string crossing a page boundary must continue in the next
/// page's frame, never past the end of this one.
fn write_user(pages: &[(u64, u64)], mut va: u64, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        let Some(phys) = page_phys(pages, va) else {
            return;
        };
        let room = (PAGE - (va & 0xFFF)) as usize;
        let (chunk, rest) = bytes.split_at(room.min(bytes.len()));
        let dst = crate::mem::phys_to_virt(PhysAddr::new(phys)) + (va & 0xFFF);
        // SAFETY: `dst..dst + chunk.len()` stays inside the one freshly
        // mapped frame backing `va`'s page (`chunk` ends at the page end).
        unsafe {
            core::ptr::copy_nonoverlapping(chunk.as_ptr(), dst.as_mut_ptr::<u8>(), chunk.len());
        }
        va += chunk.len() as u64;
        bytes = rest;
    }
}
