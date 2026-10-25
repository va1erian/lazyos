//! A ramfs file gives its heap back when it shrinks (audit #9).
//!
//! The caps count a file's length. A truncated file used to keep its whole
//! allocation, so growing one file to the cap, truncating it and growing the
//! next held heap the caps no longer saw: any user could exhaust the kernel
//! heap through `/tmp`.

use super::*;
use crate::fs::vfs::Filesystem;

const FILE: usize = 64 * 1024;
const FILES: usize = 32;

fn user() -> Id {
    Id::new(1001, 1001)
}

/// Heap bytes in use now.
fn heap_used() -> usize {
    crate::mem::heap_stats().used
}

/// Grow each of `FILES` files to `FILE` bytes, then truncate it to `keep`.
fn grow_and_truncate(ram: &RamFs, keep: u64) -> Result<(), String> {
    let data = alloc::vec![7u8; FILE];
    for index in 0..FILES {
        let name = format!("f{index}");
        if ram.lookup(&name).is_err() {
            ram.create(&name, 0o644, user()).map_err(fs_error)?;
        }
        ram.write(&name, 0, &data).map_err(fs_error)?;
        ram.truncate(&name, keep).map_err(fs_error)?;
    }
    Ok(())
}

/// After grow-then-truncate across many files, the heap holds about what
/// the files' lengths say, not every file's peak; the data that was kept
/// survives the shrink.
pub fn ramfs_shrink_returns_the_heap() -> Result<(), String> {
    // Each file fits the user's half of the cap; all of them together do not.
    let ram = RamFs::with_limits(4 * FILE, 4 * FILES);
    let before = heap_used();
    grow_and_truncate(&ram, 0)?;
    let after_empty = heap_used();
    grow_and_truncate(&ram, 100)?;
    let after_small = heap_used();
    let mut back = [0u8; 100];
    let read = ram.read("f0", 0, &mut back).map_err(fs_error)?;
    check!(
        read == 100 && back.iter().all(|&b| b == 7),
        "the kept bytes changed"
    );
    check!(
        ram.usage_of(1001).bytes == FILES * 100,
        "usage {:?}",
        ram.usage_of(1001)
    );
    // Node and name bookkeeping is a few hundred bytes a file; a retained
    // allocation would be `FILE` bytes each (2 MiB in all).
    let slack = FILES * 1024;
    check!(
        after_empty.saturating_sub(before) < slack,
        "truncating to 0 kept {} heap bytes",
        after_empty - before
    );
    check!(
        after_small.saturating_sub(before) < slack + FILES * 100,
        "truncating to 100 kept {} heap bytes",
        after_small - before
    );
    for index in 0..FILES {
        ram.unlink(&format!("f{index}")).map_err(fs_error)?;
    }
    check!(ram.usage() == (0, 1), "the tree kept {:?}", ram.usage());
    Ok(())
}

/// Soak: many grow/shrink rounds of varying sizes leave neither heap nor
/// charge behind.
pub fn ramfs_shrink_soak() -> Result<(), String> {
    let ram = RamFs::with_limits(4 * FILE, 64);
    ram.create("s", 0o644, user()).map_err(fs_error)?;
    let data = alloc::vec![3u8; FILE];
    let before = heap_used();
    for round in 0..2_000usize {
        let grow = 1 + (round * 7919) % FILE;
        let keep = (round * 104_729) % grow;
        ram.write("s", 0, &data[..grow]).map_err(fs_error)?;
        ram.truncate("s", keep as u64).map_err(fs_error)?;
        ram.truncate("s", 0).map_err(fs_error)?;
    }
    let grown = heap_used().saturating_sub(before);
    check!(grown < 4096, "{grown} heap bytes left after the soak");
    check!(
        ram.usage_of(1001).bytes == 0,
        "usage {:?}",
        ram.usage_of(1001)
    );
    ram.unlink("s").map_err(fs_error)?;
    Ok(())
}

/// A shrink to more than half keeps the allocation, and the charge says so
/// (no copy per small step, no heap the caps miss); falling under half gives
/// it back and the charge follows.
pub fn ramfs_shrink_charges_what_is_held() -> Result<(), String> {
    let ram = RamFs::with_limits(4 * FILE, 64);
    ram.create("h", 0o644, user()).map_err(fs_error)?;
    ram.write("h", 0, &alloc::vec![5u8; FILE])
        .map_err(fs_error)?;
    let mut steps = Vec::new();
    for keep in [FILE * 3 / 4, FILE * 5 / 8, FILE / 2 + 1] {
        ram.truncate("h", keep as u64).map_err(fs_error)?;
        steps.push(ram.usage_of(1001).bytes);
    }
    ram.truncate("h", (FILE / 4) as u64).map_err(fs_error)?;
    let quarter = ram.usage_of(1001).bytes;
    let (tree, _) = ram.usage();
    let mut back = [0u8; 8];
    ram.read("h", (FILE / 4 - 8) as u64, &mut back)
        .map_err(fs_error)?;
    ram.unlink("h").map_err(fs_error)?;
    check!(
        steps.iter().all(|&held| held == FILE),
        "small shrinks charged {steps:?}, expected the {FILE}-byte allocation"
    );
    check!(quarter == FILE / 4, "under half: charged {quarter}");
    check!(
        tree == quarter,
        "the tree holds {tree}, the owner {quarter}"
    );
    check!(back == [5u8; 8], "the kept bytes changed");
    Ok(())
}
