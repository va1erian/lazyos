//! Anonymous memory the machine could never back is refused where the
//! program can see it (issue #823): `mmap`, `brk` and `mremap` answer
//! `ENOMEM` when one request is larger than every frame the allocator owns,
//! and still accept what only a heuristic would call big (a reservation that
//! fits, `PROT_NONE`, `MAP_NORESERVE`), because the page fault answers a real
//! shortage with an OOM kill.

use super::*;

const ENOMEM: u64 = (-12i64) as u64;
const PROT_NONE: u64 = 0;
const MAP_NORESERVE: u64 = 0x4000;

fn total_bytes() -> u64 {
    mem::frame_stats().total as u64 * PAGE
}

fn map(len: u64, prot: u64, extra: u64) -> u64 {
    process::linux::dispatch_args5_for_test(9, 0, len, prot, MAP_PRIVATE | MAP_ANONYMOUS | extra, 0)
}

fn brk(addr: u64) -> u64 {
    process::linux::dispatch_for_test(12, addr, 0, 0)
}

/// More than all of RAM is `ENOMEM` for `mmap`, `brk` and a growing
/// `mremap`, and a refusal changes nothing; what fits, or commits nothing,
/// is accepted.
pub fn overcommit_refuses_what_could_never_fit() -> Result<(), String> {
    fresh()?;
    let table = mem::kernel_table();
    let frames = mem::frame_stats().live();
    let vmas = mem::vma::list(table).len();
    let too_big = total_bytes() + PAGE;

    for (what, prot) in [("read-write", PROT_RW), ("read-only", 1)] {
        let refused = map(too_big, prot, 0);
        check!(
            refused == ENOMEM,
            "{what} mmap of {too_big:#x}: {refused:#x}"
        );
    }
    check!(
        mem::vma::list(table).len() == vmas,
        "a refused mmap left a VMA behind"
    );

    // Nothing is committed by these, so nothing is refused: a runtime
    // reserves address space this way and commits it in pieces.
    for (what, prot, extra) in [
        ("PROT_NONE", PROT_NONE, 0),
        ("MAP_NORESERVE", PROT_RW, MAP_NORESERVE),
    ] {
        let addr = map(too_big, prot, extra);
        check!(addr as i64 > 0, "{what} mmap of {too_big:#x}: {addr:#x}");
        check!(munmap(addr, too_big) == 0, "{what}: munmap");
    }

    // A reservation well inside RAM is fine, touched or not.
    let fits = total_bytes() / 4;
    let addr = map(fits, PROT_RW, 0);
    check!(addr as i64 > 0, "mmap of a quarter of RAM: {addr:#x}");

    // mremap: growing past RAM is refused before anything moves. (Neighbouring
    // anonymous mappings coalesce, so judge the bytes mapped, not one VMA.)
    let mapped = mem::vma::mapped_bytes(table);
    let grown =
        process::linux::dispatch_args5_for_test(25, addr, fits, too_big + fits, MREMAP_MAYMOVE, 0);
    let mapped_after = mem::vma::mapped_bytes(table);
    let unmapped = munmap(addr, fits);
    check!(grown == ENOMEM, "mremap past RAM: {grown:#x}");
    check!(
        mapped_after == mapped,
        "a refused mremap changed the mapping: {mapped} -> {mapped_after} bytes"
    );
    check!(unmapped == 0, "munmap of the quarter");

    // brk: the break stays where it was.
    let start = brk(0);
    let refused = brk(start + too_big);
    check!(refused == start, "brk past RAM moved to {refused:#x}");
    let grew = brk(start + 16 * PAGE);
    check!(grew == start + 16 * PAGE, "brk by 16 pages: {grew:#x}");
    brk(start);

    let leaked = mem::frame_stats().live().saturating_sub(frames);
    check!(
        leaked <= 8,
        "{leaked} frames spent on refusals and untouched maps"
    );
    check!(mem::vma::list(table).len() == vmas, "VMAs left behind");
    Ok(())
}

/// Soak: 5,000 rounds of refusals (each call kind) and accepted-then-unmapped
/// reservations leave no VMA, charge or frame behind.
pub fn overcommit_refusals_soak() -> Result<(), String> {
    fresh()?;
    let table = mem::kernel_table();
    let frames = mem::frame_stats().live();
    let vmas = mem::vma::list(table).len();
    let too_big = total_bytes() + PAGE;
    for round in 0..5000u64 {
        let size = too_big + (round % 7) * PAGE * 1024;
        check!(
            map(size, PROT_RW, 0) == ENOMEM,
            "round {round}: mmap accepted"
        );
        let start = brk(0);
        check!(brk(start + size) == start, "round {round}: brk moved");
        let addr = map(16 * PAGE, PROT_RW, 0);
        check!(addr as i64 > 0, "round {round}: small mmap {addr:#x}");
        let grown = process::linux::dispatch_args5_for_test(
            25,
            addr,
            16 * PAGE,
            size + 16 * PAGE,
            MREMAP_MAYMOVE,
            0,
        );
        check!(grown == ENOMEM, "round {round}: mremap {grown:#x}");
        check!(munmap(addr, 16 * PAGE) == 0, "round {round}: munmap");
    }
    check!(mem::vma::list(table).len() == vmas, "VMAs left behind");
    let leaked = mem::frame_stats().live().saturating_sub(frames);
    check!(leaked <= 8, "{leaked} frames leaked");
    Ok(())
}
