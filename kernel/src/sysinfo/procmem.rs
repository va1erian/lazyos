//! op 5 of the system-stats syscall: what each process's memory is made of.
//!
//! One block of little-endian `u64`s: a [`HEADER_WORDS`]-word header and then
//! one [`ROW_WORDS`]-word row per user address space (a process and its
//! threads count once), in slot order:
//!
//! ```text
//!   header: version, row words, rows written, rows that exist, ticks
//!   row:    pid (the thread-group leader, what `getpid` says), threads,
//!           resident pages, private pages (USS), proportional bytes (PSS),
//!           page-table frames, mapped bytes,
//!           resident pages by class (image, private, stack, shared, mapped),
//!           proportional bytes by class (same order)
//! ```
//!
//! Sizes are counted the way `mem::accounting` explains: a page several
//! spaces map is divided between them, so the proportional column adds up to
//! the memory the programs hold and a program's text is counted once however
//! many processes run it. Like the snapshot, the block carries sizes only,
//! never an address, so every task may read it.

use alloc::vec::Vec;
use x86_64::PhysAddr;

use super::{errno, negative};
use crate::mem::accounting::{self, Use, CLASSES};
use crate::mem::vma;
use crate::task::{self, SpaceOwner};

/// ABI version of the block.
pub const VERSION: u64 = 1;
/// Words in the header.
pub const HEADER_WORDS: usize = 5;
/// Words in one row.
pub const ROW_WORDS: usize = 7 + 2 * CLASSES;

/// The block for the current address spaces, as words, with room for `capacity_rows` rows.
pub fn words(capacity_rows: usize) -> Vec<u64> {
    let owners = task::space_owners();
    let tables: Vec<PhysAddr> = owners.iter().map(|o| PhysAddr::new(o.pml4)).collect();
    // A buffer with no room for a row only learns how many processes there are.
    let (uses, mapped) = if capacity_rows == 0 {
        (Vec::new(), Vec::new())
    } else {
        let mapped = tables.iter().map(|t| vma::mapped_bytes(*t)).collect();
        (accounting::measure(&tables), mapped)
    };
    encode(&owners, &uses, &mapped, task::ticks(), capacity_rows)
}

/// Lay out the block: `owners` and their `uses` and `mapped` bytes (the latter two as long as the
/// rows to write), at most `capacity_rows` rows.
pub fn encode(
    owners: &[SpaceOwner],
    uses: &[Use],
    mapped: &[u64],
    ticks: u64,
    capacity_rows: usize,
) -> Vec<u64> {
    let rows = owners.len().min(capacity_rows).min(uses.len());
    let mut out = Vec::with_capacity(HEADER_WORDS + rows * ROW_WORDS);
    out.extend_from_slice(&[
        VERSION,
        ROW_WORDS as u64,
        rows as u64,
        owners.len() as u64,
        ticks,
    ]);
    for index in 0..rows {
        let (owner, used) = (&owners[index], &uses[index]);
        out.extend_from_slice(&[
            owner.pid as u64,
            owner.threads as u64,
            used.rss,
            used.private,
            used.pss,
            used.tables,
            mapped.get(index).copied().unwrap_or(0),
        ]);
        out.extend_from_slice(&used.class_rss);
        out.extend_from_slice(&used.class_pss);
    }
    out
}

/// op 5: write the block into the caller's buffer; returns the bytes written.
/// A buffer that cannot hold the header is `-E2BIG`; rows that do not fit are
/// left out (the header says how many exist).
pub fn dispatch(buf: u64, capacity: u64) -> u64 {
    if buf == 0 {
        return negative(errno::EFAULT);
    }
    let header = (HEADER_WORDS * 8) as u64;
    if capacity < header {
        return negative(errno::E2BIG);
    }
    let rows = ((capacity - header) / (ROW_WORDS as u64 * 8)) as usize;
    let words = words(rows);
    let mut bytes = Vec::with_capacity(words.len() * 8);
    for word in &words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    match crate::ipc::syscalls::copy_out(buf, &bytes) {
        Ok(()) => bytes.len() as u64,
        Err(code) => (code as u64).wrapping_neg(),
    }
}
