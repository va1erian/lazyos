//! The user-memory quota gives back exactly what was charged (audit #8).
//!
//! `brk` shrinking used to release `current - new` even when a `munmap` had
//! already returned that heap, and unmapping a stack released bytes the loader
//! never charged. Either one drained the space's other charges, so a loop of
//! them pushed the recorded usage to zero while the memory stayed mapped.

use super::mmap_quota::{used, with_quota, MIB};
use super::*;
use crate::mem::vma::{Kind, Prot};

fn brk(addr: u64) -> u64 {
    process::linux::dispatch_for_test(12, addr, 0, 0)
}

fn mmap_any(len: u64) -> u64 {
    process::linux::dispatch_args5_for_test(9, 0, len, PROT_RW, MAP_PRIVATE | MAP_ANONYMOUS, 0)
}

/// Grow the break by `len`, unmap that heap, then shrink the break back.
fn brk_unmap_shrink(len: u64) -> Result<(), String> {
    let start = brk(0);
    check!(brk(start + len) == start + len, "brk did not grow");
    check!(munmap(start, len) == 0, "munmap of the heap");
    check!(brk(start) == start, "brk did not shrink");
    Ok(())
}

/// A held mapping keeps its charge across a brk/munmap/brk round, and
/// unmapping an (uncharged) stack releases nothing.
pub fn quota_release_matches_charge() -> Result<(), String> {
    with_quota(64 * MIB, || {
        let held = mmap_any(4 * MIB);
        check!(held as i64 > 0, "mmap of the held region: {held:#x}");
        let baseline = used();

        brk_unmap_shrink(MIB)?;
        check!(
            used() == baseline,
            "brk/munmap/brk released {} bytes it had already given back",
            baseline - used()
        );

        // A stack as the loader maps it: a `Stack` VMA nobody charged.
        let table = mem::kernel_table();
        let stack = mmap_any(MIB);
        check!(stack as i64 > 0, "mmap for the stack range: {stack:#x}");
        check!(munmap(stack, MIB) == 0, "munmap of the stack range");
        mem::vma::insert(
            table,
            stack,
            stack + MIB,
            Prot::READ | Prot::WRITE,
            Kind::Stack,
        );
        check!(munmap(stack, MIB) == 0, "munmap of the stack");
        check!(
            used() == baseline,
            "unmapping an uncharged stack released {} bytes",
            baseline - used()
        );

        check!(munmap(held, 4 * MIB) == 0, "munmap of the held region");
        check!(used() == 0, "{} bytes still charged", used());
        Ok(())
    })
}

/// Soak: thousands of brk/munmap/brk rounds never move the charge of a
/// mapping that stays held.
pub fn quota_release_soak() -> Result<(), String> {
    with_quota(64 * MIB, || {
        let held = mmap_any(8 * MIB);
        check!(held as i64 > 0, "mmap of the held region: {held:#x}");
        let baseline = used();
        for round in 0..2_000u32 {
            brk_unmap_shrink(16 * PAGE).map_err(|error| format!("round {round}: {error}"))?;
            check!(
                used() == baseline,
                "round {round}: charge moved {baseline} -> {}",
                used()
            );
        }
        check!(munmap(held, 8 * MIB) == 0, "munmap of the held region");
        check!(used() == 0, "{} bytes still charged", used());
        Ok(())
    })
}
