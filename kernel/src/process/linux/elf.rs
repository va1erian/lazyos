//! Loading a static Linux ELF64 image and building its initial process start
//! stack (`argc`/`argv`/`envp`/`auxv`) with [`load_image`], shared by the
//! kernel's own spawns (`task::spawn_linux*`) and by `execve`
//! (`procctl::sys_execve`), which replaces a running image the same way.
//!
//! The main-thread stack is `limit.stack_size` bytes below
//! [`STACK_TOP`](super::STACK_TOP) (8 MiB by default, like Linux's
//! `ulimit -s`). Only the pages the start frame occupies are mapped up front;
//! the rest of the `Stack` VMA is demand-zero, so a deep stack costs nothing
//! until it is used and a fault below it is a `SIGSEGV`, never another
//! mapping.

use alloc::vec::Vec;

use x86_64::PhysAddr;

use crate::mem::vma::{Kind, Prot};
use crate::process::image::{self, Image};
use crate::process::layout::{IMAGE_RESERVED, STACK_MAX, STACK_TOP};
use crate::process::{load_segments, loader, map_range_kind, page_phys, Loaded};

use super::errno::{EIO, ENOEXEC, ENOMEM};
use super::uaccess::fill_random;
use super::PAGE;

/// Where a freshly loaded image starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Started {
    pub entry: u64,
    /// The initial stack pointer (at `argc`).
    pub rsp: u64,
    /// The first heap address: `brk` starts here.
    pub brk: u64,
}

/// Why a load failed, for the errno a caller reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The image itself is unloadable (`ENOEXEC`).
    BadImage(&'static str),
    /// Frames ran out (`ENOMEM`).
    NoMemory,
    /// The filesystem failed to read the image (`EIO`).
    Io,
}

impl LoadError {
    /// The errno `execve` reports for this failure.
    pub fn errno(self) -> u64 {
        match self {
            LoadError::BadImage(_) => ENOEXEC,
            LoadError::NoMemory => ENOMEM,
            LoadError::Io => EIO,
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            LoadError::BadImage(reason) => reason,
            LoadError::NoMemory => loader::OUT_OF_MEMORY,
            LoadError::Io => image::READ_FAILED,
        }
    }
}

/// Map a loader reason to the errno class it stands for: exact matches only,
/// so a validation message that happens to start like one stays `ENOEXEC`.
pub fn classify(reason: &'static str) -> LoadError {
    if loader::is_out_of_memory(reason) {
        LoadError::NoMemory
    } else if reason == image::READ_FAILED {
        LoadError::Io
    } else {
        LoadError::BadImage(reason)
    }
}

/// The main-thread stack size this load uses: the configured limit, page
/// aligned and inside the layout's room for it.
pub fn stack_size() -> u64 {
    crate::limits::stack_size().min(STACK_MAX) & !(PAGE - 1)
}

/// Load `image` into `table`: map its segments and its stack and build the
/// start stack. `argv`/`envp` are NUL-terminated; `ids` is the `(uid, gid)`
/// the auxiliary vector reports. On error `table` may hold a partial image,
/// which the caller frees with the table.
pub fn load_image<I: Image + ?Sized>(
    table: PhysAddr,
    image: &I,
    argv: &[Vec<u8>],
    envp: &[Vec<u8>],
    ids: (u32, u32),
) -> Result<Started, LoadError> {
    let loaded = load_segments(table, image, &IMAGE_RESERVED).map_err(classify)?;
    let frame = start_frame_bytes(argv, envp);
    let size = stack_size().max(frame);
    let bottom = STACK_TOP - size;
    // Map the pages the start frame needs; the rest of the stack is a
    // demand-zero extension of the same VMA.
    let eager = STACK_TOP - frame;
    let stack = map_range_kind(
        table,
        eager,
        STACK_TOP,
        Prot::READ | Prot::WRITE,
        Kind::Stack,
    )
    .map_err(|_| LoadError::NoMemory)?;
    crate::mem::vma::insert(
        table,
        bottom,
        STACK_TOP,
        Prot::READ | Prot::WRITE,
        Kind::Stack,
    );
    let rsp = build_start_stack(&stack, argv, envp, &loaded, ids);
    Ok(Started {
        entry: loaded.entry,
        rsp,
        brk: crate::process::layout::heap_start(loaded.end, 0),
    })
}

/// Each item followed by a NUL, as the start stack stores strings.
pub fn nul_terminated(items: &[&[u8]]) -> Vec<Vec<u8>> {
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

/// Number of `(type, value)` pairs [`build_start_stack`] writes, `AT_NULL`
/// included.
const AUXV_PAIRS: u64 = 14;

/// Page-rounded bytes the start frame occupies below [`STACK_TOP`]: the 16
/// random bytes, every string, the word arrays and alignment slack, plus a
/// page so the program's first frames do not fault immediately.
fn start_frame_bytes(argv: &[Vec<u8>], envp: &[Vec<u8>]) -> u64 {
    let strings: u64 = argv.iter().chain(envp).map(|s| s.len() as u64).sum::<u64>()
        + argv.first().map_or(0, |a| a.len() as u64);
    let words = 1 + argv.len() as u64 + 1 + envp.len() as u64 + 1 + 2 * AUXV_PAIRS;
    let bytes = 16 + strings + words * 8 + 16 + PAGE;
    (bytes + PAGE - 1) & !(PAGE - 1)
}

/// Build the Linux process start stack: `argc/argv/envp/auxv` plus strings.
/// `argv`/`envp` are NUL-terminated byte strings; `ids` is the `(uid, gid)`
/// reported through `AT_UID`/`AT_EUID`/`AT_GID`/`AT_EGID`.
pub(super) fn build_start_stack(
    stack: &[(u64, u64)],
    argv: &[Vec<u8>],
    envp: &[Vec<u8>],
    loaded: &Loaded,
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
    let auxv: [(u64, u64); AUXV_PAIRS as usize - 1] = [
        (AT_PHDR, loaded.phdr),
        (AT_PHENT, u64::from(loaded.phent)),
        (AT_PHNUM, u64::from(loaded.phnum)),
        (AT_PAGESZ, PAGE),
        (AT_BASE, 0),
        (AT_ENTRY, loaded.entry),
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
