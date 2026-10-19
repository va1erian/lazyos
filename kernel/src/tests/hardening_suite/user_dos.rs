//! Two user-triggerable denial-of-service classes (2026-10 core audit, H3 and
//! H4): field offsets added to a raw user pointer (`ptr + 8` panics on
//! overflow, the default profile), and `munmap`/`mprotect`/`madvise`/`mremap`
//! over ranges so large that a per-page walk with interrupts off never ends.

use super::*;
use crate::process::layout::{MMAP_BASE, MMAP_LIMIT};
use crate::user_ptr;

const ENOMEM: i64 = 12;
const EINVAL: i64 = 22;

const SYS_MMAP: u64 = 9;
const SYS_MPROTECT: u64 = 10;
const SYS_MUNMAP: u64 = 11;
const SYS_MREMAP: u64 = 25;
const SYS_MADVISE: u64 = 28;

const PROT_NONE: u64 = 0;
const PROT_RW: u64 = 3;
/// `MAP_PRIVATE | MAP_FIXED | MAP_ANONYMOUS | MAP_NORESERVE`.
const MAP_RESERVE_FIXED: u64 = 0x02 | 0x10 | 0x20 | 0x4000;
const MADV_DONTNEED: u64 = 4;
const PAGE: u64 = 4096;

/// A huge reservation call must come back in well under this; the walk it
/// replaces would take years.
const FAST_NS: u64 = 5_000_000_000;

/// Whether `va` has a present leaf (MMIO leaves included, which
/// `resident_pages` does not count).
fn mapped(table: x86_64::PhysAddr, va: u64) -> bool {
    mem::pte_chain(table, va)[3] & mem::pte::PRESENT != 0
}

fn mmap_fixed(addr: u64, len: u64, prot: u64) -> u64 {
    process::linux::dispatch_args6_for_test(
        SYS_MMAP,
        [addr, len, prot, MAP_RESERVE_FIXED, u64::MAX, 0],
    )
}

/// Pointers whose first word fits nowhere (or nearly) make the unchecked
/// `ptr + 8` of every handler below overflow: each must be `-EFAULT`, not a
/// kernel panic, and a failed `pipe` must not leak its descriptors.
pub fn user_pointer_field_offsets_do_not_overflow() -> Result<(), String> {
    fresh()?;
    let _strict = Strict::on();
    let bad = [u64::MAX, u64::MAX - 3, u64::MAX - 7, u64::MAX - 15];
    for ptr in bad {
        // clock_gettime(CLOCK_MONOTONIC), clock_getres, gettimeofday,
        // nanosleep, clock_nanosleep.
        for (nr, a1, a2, a3, label) in [
            (228, 1, ptr, 0, "clock_gettime"),
            (229, 1, ptr, 0, "clock_getres"),
            (96, ptr, 0, 0, "gettimeofday"),
            (35, ptr, 0, 0, "nanosleep"),
            (230, 1, 0, ptr, "clock_nanosleep"),
            (227, 0, ptr, 0, "clock_settime"),
            (22, ptr, 0, 0, "pipe"),
            (293, ptr, 0, 0, "pipe2"),
            (38, 0, ptr, 0, "setitimer"),
            (97, 7, ptr, 0, "getrlimit"),
            (160, 7, ptr, 0, "setrlimit"),
            (23, 1, 0, 0, "select"),
        ] {
            // select takes its timeout as the fifth argument.
            let code = if nr == 23 {
                process::linux::dispatch_args5_for_test(23, 0, 0, 0, 0, ptr)
            } else {
                process::linux::dispatch_for_test(nr, a1, a2, a3)
            };
            check!(
                code == failed(EFAULT),
                "{label}({ptr:#x}) -> {code:#x}, expected -EFAULT"
            );
        }
        let code = process::linux::dispatch_args_for_test(53, 1, 1, 0, ptr);
        check!(
            code == failed(EFAULT),
            "socketpair(sv={ptr:#x}) -> {code:#x}, expected -EFAULT"
        );
    }

    // Pointers that only the *offset* takes over the edge, in handlers that
    // degrade instead of failing: they must simply return.
    for ptr in bad {
        let _ = process::linux::dispatch_args_for_test(13, 2, ptr, 0, 8); // rt_sigaction
        let _ = process::linux::dispatch_for_test(131, ptr, ptr, 0); // sigaltstack
        let _ = process::linux::dispatch_for_test(5, 0, ptr, 0); // fstat
    }

    // A pipe that cannot be reported is closed again: no descriptor leaks no
    // matter how many times it is asked for.
    for round in 0..20_000 {
        let code = process::linux::dispatch_for_test(293, u64::MAX, 0, 0);
        check!(
            code == failed(EFAULT),
            "pipe2 round {round} -> {code:#x} (a leak ends in -EMFILE)"
        );
    }

    // A robust-list head whose second word overflows ends the exit walk
    // quietly.
    let code = process::linux::dispatch_for_test(273, u64::MAX, 24, 0);
    check!(code == 0, "set_robust_list -> {code:#x}");
    process::linux::robust_exit_for_test(task::current());

    // futex: a timeout pointer at the edge, a valid futex word.
    in_space(|| -> Result<(), String> {
        let word = SPACE + 0x100;
        let code = process::linux::dispatch_args_for_test(202, word, 0, 0, u64::MAX);
        check!(code == failed(EFAULT), "futex wait timeout -> {code:#x}");
        Ok(())
    })?;

    // The positive control: a timespec that straddles a page boundary.
    in_space(|| -> Result<(), String> {
        let at = SPACE + PAGE - 8;
        let code = process::linux::dispatch_for_test(228, 1, at, 0);
        check!(
            code == 0,
            "clock_gettime into a mapped timespec -> {code:#x}"
        );
        let seconds = user_ptr::try_read::<i64>(at).map_err(|_| "read seconds")?;
        let nanos = user_ptr::try_read_field::<i64>(at, 8).map_err(|_| "read nanos")?;
        check!(
            seconds >= 0 && (0..1_000_000_000).contains(&nanos),
            "timespec {seconds}.{nanos}"
        );
        // A null `tv` / `res` is allowed.
        check!(
            process::linux::dispatch_for_test(96, 0, 0, 0) == 0,
            "gettimeofday(NULL)"
        );
        check!(
            process::linux::dispatch_for_test(229, 1, 0, 0) == 0,
            "clock_getres(NULL)"
        );
        Ok(())
    })?;
    Ok(())
}

/// The helpers themselves: an overflowing offset is a fault, never a wrap.
pub fn user_ptr_field_helpers_check_overflow() -> Result<(), String> {
    check!(user_ptr::field(8, 8) == Ok(16), "field(8, 8)");
    check!(user_ptr::field(u64::MAX, 1).is_err(), "field(MAX, 1)");
    check!(user_ptr::field(u64::MAX - 7, 8).is_err(), "field(MAX-7, 8)");
    check!(user_ptr::at(u64::MAX - 3, 8) == u64::MAX, "at saturates");
    check!(user_ptr::at(16, 8) == 24, "at(16, 8)");
    check!(
        user_ptr::try_read_field::<u64>(u64::MAX - 3, 4).is_err(),
        "try_read_field overflow"
    );
    check!(
        user_ptr::try_write_field::<u64>(u64::MAX - 3, 4, 1).is_err(),
        "try_write_field overflow"
    );
    Ok(())
}

/// Huge lengths: beyond the user window they are an error at once, inside it
/// a mostly absent range is skipped by table, not walked by page.
pub fn huge_memory_ranges_return_fast() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        // Beyond the user window.
        let started = crate::arch::clock::monotonic_ns();
        for (nr, a1, a2, a3, want, label) in [
            (SYS_MUNMAP, SPACE, 1u64 << 63, 0, EINVAL, "munmap(2^63)"),
            (SYS_MUNMAP, SPACE, 1u64 << 62, 0, EINVAL, "munmap(2^62)"),
            (
                SYS_MPROTECT,
                SPACE,
                1u64 << 63,
                PROT_RW,
                ENOMEM,
                "mprotect(2^63)",
            ),
            (
                SYS_MADVISE,
                SPACE,
                1u64 << 63,
                MADV_DONTNEED,
                ENOMEM,
                "madvise(2^63)",
            ),
            (
                SYS_MUNMAP,
                MMAP_BASE,
                mem::USER_TOP,
                0,
                EINVAL,
                "munmap(past USER_TOP)",
            ),
        ] {
            let code = process::linux::dispatch_for_test(nr, a1, a2, a3);
            check!(
                code == failed(want),
                "{label} -> {code:#x}, expected {want}"
            );
        }
        let code =
            process::linux::dispatch_args5_for_test(SYS_MREMAP, 0x0040_0000, 4096, 1 << 62, 1, 0);
        check!(
            code == failed(ENOMEM) || code == failed(EINVAL),
            "mremap(2^62) -> {code:#x}"
        );
        check!(
            crate::arch::clock::monotonic_ns() - started < FAST_NS,
            "refusing a huge length was slow"
        );

        // A reservation of the whole mmap area, protected, touched, advised
        // and unmapped: every step is table-sized work.
        let len = MMAP_LIMIT - MMAP_BASE;
        let started = crate::arch::clock::monotonic_ns();
        let base = mmap_fixed(MMAP_BASE, len, PROT_NONE);
        check!(base == MMAP_BASE, "reservation -> {base:#x}");
        let code = process::linux::dispatch_for_test(SYS_MPROTECT, MMAP_BASE, len, PROT_RW);
        check!(code == 0, "mprotect(reservation) -> {code:#x}");
        let _strict = Strict::on();
        let touched = [MMAP_BASE, MMAP_BASE + (1 << 40) + PAGE, MMAP_LIMIT - PAGE];
        for &page in &touched {
            user_ptr::try_write::<u64>(page, 0x5eed).map_err(|_| format!("touch {page:#x}"))?;
        }
        let table = mem::kernel_table();
        check!(
            mem::resident_pages(table, MMAP_BASE, MMAP_LIMIT) == touched.len() as u64,
            "resident pages after touching"
        );
        let code = process::linux::dispatch_for_test(SYS_MADVISE, MMAP_BASE, len, MADV_DONTNEED);
        check!(code == 0, "madvise(DONTNEED, reservation) -> {code:#x}");
        check!(
            mem::resident_pages(table, MMAP_BASE, MMAP_LIMIT) == 0,
            "DONTNEED left pages resident"
        );
        user_ptr::try_write::<u64>(MMAP_BASE + (1 << 41), 1).map_err(|_| "re-touch")?;
        let code = process::linux::dispatch_for_test(SYS_MPROTECT, MMAP_BASE, len, PROT_NONE);
        check!(code == 0, "mprotect(NONE, reservation) -> {code:#x}");
        let code = process::linux::dispatch_for_test(SYS_MUNMAP, MMAP_BASE, len, 0);
        check!(code == 0, "munmap(reservation) -> {code:#x}");
        check!(
            mem::resident_pages(table, MMAP_BASE, MMAP_LIMIT) == 0
                && mem::vma::find_range(table, MMAP_BASE, MMAP_LIMIT).is_empty(),
            "munmap left pages or VMAs behind"
        );
        check!(
            crate::arch::clock::monotonic_ns() - started < FAST_NS,
            "a huge reservation's calls were slow"
        );
        Ok(())
    })
}

/// A reservation that grows in place may not run past the mmap area, and a
/// huge relocation of a sparse mapping is table-sized work.
pub fn mremap_is_bounded_by_the_mmap_area() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let len = 1u64 << 40;
        let at = MMAP_LIMIT - len;
        check!(mmap_fixed(at, len, PROT_RW) == at, "reservation at the top");
        // MREMAP_MAYMOVE = 1: growing past MMAP_LIMIT cannot happen in place
        // (the old code extended the VMA over the MMIO window and beyond).
        let grown = len + (1 << 30);
        let code = process::linux::dispatch_args5_for_test(SYS_MREMAP, at, len, grown, 0, 0);
        check!(
            code == failed(ENOMEM),
            "in-place growth past MMAP_LIMIT -> {code:#x}"
        );
        // Relocating the sparse mapping (MAYMOVE) is fast.
        let started = crate::arch::clock::monotonic_ns();
        let _strict = Strict::on();
        user_ptr::try_write::<u64>(at + (1 << 30), 7).map_err(|_| "touch")?;
        let moved = process::linux::dispatch_args5_for_test(SYS_MREMAP, at, len, grown, 1, 0);
        check!(
            moved != at && moved < MMAP_LIMIT,
            "mremap(MAYMOVE) -> {moved:#x}"
        );
        check!(
            user_ptr::try_read::<u64>(moved + (1 << 30)) == Ok(7),
            "the resident page did not move with the mapping"
        );
        check!(
            crate::arch::clock::monotonic_ns() - started < FAST_NS,
            "relocating a sparse mapping was slow"
        );
        let code = process::linux::dispatch_for_test(SYS_MUNMAP, moved, grown, 0);
        check!(code == 0, "munmap(moved) -> {code:#x}");
        check!(
            mem::vma::find_range(mem::kernel_table(), MMAP_BASE, MMAP_LIMIT).is_empty(),
            "the relocated mapping left a VMA behind"
        );
        Ok(())
    })
}

/// A device BAR leaf is not an allocator frame: `munmap` (and
/// `madvise`) over it leaves it, and its frame, alone.
pub fn unmap_range_skips_device_leaves() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let table = mem::kernel_table();
        let va = MMAP_LIMIT + (1 << 20);
        let phys = 0xFD00_0000;
        check!(mem::mmio::map_mmio(table, va, phys, 4), "map_mmio failed");
        let cleared = mem::unmap_range(table, va, va + 4 * PAGE);
        check!(cleared == 0, "unmap_range cleared {cleared} MMIO leaves");
        check!(
            (0..4).all(|page| mapped(table, va + page * PAGE)),
            "an MMIO leaf was unmapped by unmap_range"
        );
        // The owner removes it, as `dev::ops` does.
        check!(mem::mmio::unmap_mmio(table, va, phys, 4) == 4, "unmap_mmio");
        check!(
            (0..4).all(|page| !mapped(table, va + page * PAGE)),
            "leaves left"
        );
        Ok(())
    })
}

/// Soak: reservations of random size at random places, protected, touched,
/// advised and unmapped, plus unmaps of ranges nothing maps, over thousands
/// of rounds; nothing may stay behind and nothing may take long.
pub fn soak_sparse_huge_ranges() -> Result<(), String> {
    fresh()?;
    in_space(|| -> Result<(), String> {
        let table = mem::kernel_table();
        let _strict = Strict::on();
        let started = crate::arch::clock::monotonic_ns();
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for round in 0..300 {
            let span = (MMAP_LIMIT - MMAP_BASE) / PAGE;
            let len = ((next() % (1 << 33)) + 1) * PAGE; // up to 32 TiB
            let base = MMAP_BASE + (next() % span) * PAGE;
            let len = len.min(MMAP_LIMIT - base);
            let got = mmap_fixed(base, len, PROT_NONE);
            check!(got == base, "round {round}: reservation -> {got:#x}");
            let code = process::linux::dispatch_for_test(SYS_MPROTECT, base, len, PROT_RW);
            check!(code == 0, "round {round}: mprotect -> {code:#x}");
            let touches = next() % 4;
            for _ in 0..touches {
                let page = base + (next() % (len / PAGE)) * PAGE;
                user_ptr::try_write::<u64>(page, round).map_err(|_| format!("touch {page:#x}"))?;
            }
            if next() % 2 == 0 {
                let code = process::linux::dispatch_for_test(SYS_MADVISE, base, len, MADV_DONTNEED);
                check!(code == 0, "round {round}: madvise -> {code:#x}");
            }
            let code = process::linux::dispatch_for_test(SYS_MUNMAP, base, len, 0);
            check!(code == 0, "round {round}: munmap -> {code:#x}");
            // Nothing is mapped there now: unmapping or protecting it again
            // is a quick no-op.
            let code = process::linux::dispatch_for_test(SYS_MUNMAP, base, len, 0);
            check!(code == 0, "round {round}: second munmap -> {code:#x}");
            let code = process::linux::dispatch_for_test(SYS_MPROTECT, base, len, PROT_NONE);
            check!(code == 0, "round {round}: mprotect of nothing -> {code:#x}");
        }
        check!(
            mem::resident_pages(table, MMAP_BASE, MMAP_LIMIT) == 0
                && mem::vma::find_range(table, MMAP_BASE, MMAP_LIMIT).is_empty(),
            "the soak left pages or VMAs mapped"
        );
        check!(
            crate::arch::clock::monotonic_ns() - started < 10 * FAST_NS,
            "the sparse-range soak was slow"
        );
        Ok(())
    })
}
