//! A reservation (`PROT_NONE`, `MAP_NORESERVE`) is charged to the user-memory
//! quota by the pages that come to be resident, not by its length: a runtime
//! that sets aside gigabytes of address space and fills it in pieces pays for
//! the pieces (WebKit's 1 GiB code pool and 8 GiB structure heap were
//! refusing a second page process under the 8 GiB quota). The charge is taken
//! at the first touch (with the page-table frames the touch has to create,
//! which stay charged until the address space goes), given back as the pages
//! are unmapped, dropped or moved, and a uid at its quota is refused the
//! touch, not the reservation.

use super::*;
use crate::quota;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::structures::idt::PageFaultErrorCode;

const PROT_NONE: u64 = 0;
const MAP_NORESERVE: u64 = 0x4000;
const MADV_DONTNEED: u64 = 4;
const SYS_GETRANDOM: u64 = 318;
const GIB: u64 = 1 << 30;
const ENOMEM: u64 = (-12i64) as u64;

/// Page-table frames the touches of this test charged (they are not given back
/// by `munmap`: the tables stay until the address space is freed).
static TABLES: AtomicU64 = AtomicU64::new(0);

fn tables() -> u64 {
    TABLES.load(Ordering::Relaxed) * PAGE
}

fn map(len: u64, prot: u64, extra: u64) -> u64 {
    process::linux::dispatch_args5_for_test(9, 0, len, prot, MAP_PRIVATE | MAP_ANONYMOUS | extra, 0)
}

fn mprotect(addr: u64, len: u64, prot: u64) -> u64 {
    process::linux::dispatch_for_test(10, addr, len, prot)
}

fn madvise(addr: u64, len: u64, advice: u64) -> u64 {
    process::linux::dispatch_for_test(28, addr, len, advice)
}

/// Touch the page at `va` as a write fault does; whether it came to be.
fn touch(va: u64) -> bool {
    let table = mem::kernel_table();
    let missing = mem::missing_tables(table, va);
    let came = mem::demand_fault(table, va, PageFaultErrorCode::CAUSED_BY_WRITE);
    if came {
        TABLES.fetch_add(missing, Ordering::Relaxed);
    }
    came
}

/// `getrandom(addr, len)` with the user-pointer validation on, as a ring-3
/// caller meets it (the harness trusts kernel pointers by default, which would
/// turn a refused page into a raw kernel-mode fault instead of `EFAULT`).
fn getrandom_into(addr: u64, len: u64) -> u64 {
    let trust = crate::user_ptr::set_trust_kernel_pointers(false);
    let result = process::linux::dispatch_for_test(SYS_GETRANDOM, addr, len, 0);
    crate::user_ptr::set_trust_kernel_pointers(trust);
    result
}

fn resident(start: u64, len: u64) -> u64 {
    mem::resident_pages(mem::kernel_table(), start, start + len)
}

/// A uid's quota run: the table charge starts from nothing.
fn quota_run(limit: u64, body: impl FnOnce() -> Result<(), String>) -> Result<(), String> {
    TABLES.store(0, Ordering::Relaxed);
    with_quota(limit, body)
}

/// Gigabytes of reservation cost nothing under a 64 MiB quota; each page
/// touched costs one page (and its tables); `madvise(DONTNEED)`, `munmap` and a
/// `MAP_FIXED` replacement give back exactly the pages that were resident, and
/// a reservation that was made read-write by `mprotect` stays charged by page.
pub fn mmap_reservation_is_charged_by_resident_pages() -> Result<(), String> {
    quota_run(64 * MIB, || {
        let frames = mem::frame_stats().live();
        let region = map(GIB, PROT_RW, MAP_NORESERVE);
        check!(region as i64 > 0, "NORESERVE mmap of 1 GiB: {region:#x}");
        let none = map(GIB, PROT_NONE, 0);
        check!(none as i64 > 0, "PROT_NONE mmap of 1 GiB: {none:#x}");
        check!(used() == 0, "{} bytes charged for two reservations", used());

        // Sixteen pages, three apart.
        for index in 0..16 {
            let va = region + index * 3 * PAGE;
            check!(touch(va), "touch of reserved page {index} at {va:#x}");
        }
        let want = 16 * PAGE + tables();
        check!(used() == want, "{} bytes charged, expected {want}", used());
        check!(
            resident(region, GIB) == 16,
            "{} resident",
            resident(region, GIB)
        );

        // PROT_NONE: nothing to touch, nothing charged; `mprotect` commits
        // nothing either, the pages are charged as they are touched.
        check!(!touch(none), "a PROT_NONE page came to be");
        check!(mprotect(none, 64 * PAGE, PROT_RW) == 0, "mprotect");
        check!(used() == want, "mprotect charged: {}", used());
        for index in 0..4 {
            check!(touch(none + index * PAGE), "touch {index} after mprotect");
        }
        check!(used() == 20 * PAGE + tables(), "{} bytes after 4", used());
        check!(
            !touch(none + 100 * PAGE),
            "a page still PROT_NONE came to be"
        );
        check!(
            used() == 20 * PAGE + tables(),
            "a refused touch charged: {}",
            used()
        );

        // MADV_DONTNEED drops the pages and their charge, not the range.
        check!(madvise(none, 64 * PAGE, MADV_DONTNEED) == 0, "madvise");
        check!(used() == 16 * PAGE + tables(), "{} after DONTNEED", used());
        check!(touch(none), "a dropped page did not fault back in");
        check!(used() == 17 * PAGE + tables(), "{} after refault", used());

        // munmap of the first 24 pages: 8 of the 16 touched pages lived there.
        check!(munmap(region, 24 * PAGE) == 0, "munmap");
        check!(used() == 9 * PAGE + tables(), "{} after munmap", used());

        // MAP_FIXED over the other 8: the new mapping is charged by length and
        // the replaced reservation gives back its 8 resident pages.
        let fixed = mmap_fixed(region + 24 * PAGE, 32 * PAGE);
        check!(fixed == region + 24 * PAGE, "MAP_FIXED gave {fixed:#x}");
        check!(
            used() == (1 + 32) * PAGE + tables(),
            "{} after MAP_FIXED",
            used()
        );
        check!(munmap(fixed, 32 * PAGE) == 0, "munmap of the fixed mapping");
        check!(used() == PAGE + tables(), "{} with one page left", used());

        check!(munmap(none, GIB) == 0, "munmap of the PROT_NONE range");
        check!(
            munmap(region + 56 * PAGE, GIB - 56 * PAGE) == 0,
            "munmap of the tail"
        );
        check!(used() == tables(), "{} still charged", used());
        check!(quota::stats(UID).over_releases == 0, "released twice");
        let leaked = mem::frame_stats().live().saturating_sub(frames);
        check!(leaked <= 24, "{leaked} frames left (page tables aside)");
        Ok(())
    })
}

/// A uid at its quota can still reserve (a reservation commits nothing) and is
/// refused the touch that would exceed it, with nothing mapped or charged;
/// counted memory is refused at `mmap` as before; unmapping makes room again.
pub fn mmap_reservation_at_the_quota_refuses_the_touch() -> Result<(), String> {
    const LIMIT: u64 = 24 * PAGE;
    quota_run(LIMIT, || {
        let region = map(GIB, PROT_RW, MAP_NORESERVE);
        check!(
            region as i64 > 0,
            "reservation under a tiny quota: {region:#x}"
        );
        check!(touch(region), "the first page, with its tables");
        // Everything below the limit now fits, one page each (one page table
        // covers them all).
        let room = (LIMIT - used()) / PAGE;
        for index in 1..=room {
            check!(touch(region + index * PAGE), "touch {index} of {room}");
        }
        check!(used() == LIMIT, "{} bytes charged of {LIMIT}", used());
        let frames = mem::frame_stats().live();
        let beyond = region + (room + 1) * PAGE;
        check!(!touch(beyond), "a page past the quota came to be");
        check!(used() == LIMIT, "the refusal charged: {}", used());
        check!(
            resident(region, GIB) == room + 1,
            "the refused page is mapped"
        );
        check!(
            mem::frame_stats().live() <= frames,
            "the refused touch kept a frame"
        );
        check!(quota::stats(UID).denials >= 1, "the denial was not counted");

        // More reservations are fine; counted memory is not.
        let more = map(GIB, PROT_NONE, 0);
        check!(
            more as i64 > 0,
            "a second reservation at the quota: {more:#x}"
        );
        check!(map(PAGE, PROT_RW, 0) == ENOMEM, "a counted page fitted");

        // A syscall that writes the page is refused, not killed.
        let wrote = getrandom_into(beyond, 16);
        check!(
            (wrote as i64) < 0,
            "getrandom into a refused page: {wrote:#x}"
        );
        check!(used() == LIMIT, "the syscall charged: {}", used());

        check!(munmap(region + PAGE, 4 * PAGE) == 0, "munmap");
        check!(used() == LIMIT - 4 * PAGE, "{} after munmap", used());
        let wrote = getrandom_into(beyond, 16);
        check!(wrote == 16, "getrandom with room again: {wrote:#x}");
        check!(used() == LIMIT - 3 * PAGE, "the syscall's page: {}", used());
        check!(
            munmap(region, GIB) == 0 && munmap(more, GIB) == 0,
            "cleanup"
        );
        check!(used() == tables(), "{} still charged", used());
        check!(quota::stats(UID).over_releases == 0, "released twice");
        Ok(())
    })
}

/// `mremap` keeps a reservation's charge with its pages: growing in place or
/// moving costs nothing; a shrink gives back the resident pages of the tail,
/// and so does a move that is also a shrink (`MREMAP_FIXED`), which also gives
/// back the reservation it replaces.
pub fn mmap_reservation_follows_mremap() -> Result<(), String> {
    quota_run(64 * MIB, || {
        // A counted page right above forces a relocation: it is made by
        // replacing the last page of a 65-page reservation.
        let region = map(65 * PAGE, PROT_RW, MAP_NORESERVE);
        check!(region as i64 > 0, "mmap: {region:#x}");
        let blocker = mmap_fixed(region + 64 * PAGE, PAGE);
        check!(blocker == region + 64 * PAGE, "blocker: {blocker:#x}");
        for index in 0..8 {
            check!(touch(region + index * PAGE), "touch {index}");
        }
        let moved = mremap(region, 64 * PAGE, 4096 * PAGE, MREMAP_MAYMOVE, 0);
        check!(
            moved as i64 > 0 && moved != region,
            "mremap move: {moved:#x}"
        );
        check!(
            used() == 8 * PAGE + PAGE + tables(),
            "{} bytes after moving",
            used()
        );
        check!(resident(moved, 4096 * PAGE) == 8, "pages did not move");
        check!(resident(region, 64 * PAGE) == 0, "pages stayed behind");

        // A shrink in place drops the tail's four resident pages.
        let shrunk = mremap(moved, 4096 * PAGE, 4 * PAGE, MREMAP_MAYMOVE, 0);
        check!(shrunk == moved, "mremap shrink: {shrunk:#x}");
        check!(
            used() == 4 * PAGE + PAGE + tables(),
            "{} bytes after shrinking",
            used()
        );

        // A shrinking move onto another reservation: the four pages that stay
        // move, the four in the tail are dropped, the target's four go.
        let src = map(64 * PAGE, PROT_RW, MAP_NORESERVE);
        let dst = map(64 * PAGE, PROT_RW, MAP_NORESERVE);
        check!(src as i64 > 0 && dst as i64 > 0, "mmap of the pair");
        for index in 0..8 {
            check!(touch(src + index * PAGE), "touch src {index}");
        }
        for index in 0..4 {
            check!(touch(dst + index * PAGE), "touch dst {index}");
        }
        let before = used();
        let flags = MREMAP_MAYMOVE | MREMAP_FIXED;
        let at = mremap(src, 64 * PAGE, 4 * PAGE, flags, dst);
        check!(at == dst, "MREMAP_FIXED gave {at:#x}");
        check!(used() == before - 8 * PAGE, "{} (from {before})", used());
        check!(
            resident(dst, 4 * PAGE) == 4,
            "the kept pages did not arrive"
        );
        check!(resident(src, 64 * PAGE) == 0, "pages stayed in the source");

        check!(munmap(dst, 64 * PAGE) == 0, "munmap of the target");
        check!(munmap(moved, 4 * PAGE) == 0, "munmap of the moved range");
        check!(munmap(blocker, PAGE) == 0, "munmap of the blocker");
        check!(used() == tables(), "{} bytes still charged", used());
        check!(quota::stats(UID).over_releases == 0, "released twice");
        Ok(())
    })
}

/// A range that spans a reservation and counted memory releases each by its
/// own rule: the resident pages of the one, the length of the other.
pub fn mmap_reservation_beside_counted_memory() -> Result<(), String> {
    quota_run(64 * MIB, || {
        // One 16-page reservation whose upper half is replaced by counted
        // memory: the two VMAs touch.
        let reserved = map(16 * PAGE, PROT_RW, MAP_NORESERVE);
        check!(reserved as i64 > 0, "mmap: {reserved:#x}");
        for index in [2, 5, 6] {
            check!(touch(reserved + index * PAGE), "touch {index}");
        }
        let counted = mmap_fixed(reserved + 8 * PAGE, 8 * PAGE);
        check!(counted == reserved + 8 * PAGE, "MAP_FIXED: {counted:#x}");
        check!(used() == 3 * PAGE + 8 * PAGE + tables(), "{}", used());

        // The upper half of the reservation (two resident pages) and the
        // lower half of the counted mapping (four pages by length).
        check!(munmap(reserved + 4 * PAGE, 8 * PAGE) == 0, "munmap across");
        check!(
            used() == PAGE + 4 * PAGE + tables(),
            "{} after the span",
            used()
        );
        check!(munmap(reserved, 4 * PAGE) == 0, "munmap of the lower half");
        check!(
            munmap(reserved + 12 * PAGE, 4 * PAGE) == 0,
            "munmap of the upper counted half"
        );
        check!(used() == tables(), "{} still charged", used());
        check!(quota::stats(UID).over_releases == 0, "released twice");
        Ok(())
    })
}

/// A syscall that writes a reserved page charges it like a fault does.
pub fn mmap_reservation_is_charged_by_syscall_writes() -> Result<(), String> {
    quota_run(64 * MIB, || {
        let region = map(16 * PAGE, PROT_RW, MAP_NORESERVE);
        check!(region as i64 > 0, "mmap: {region:#x}");
        let missing = mem::missing_tables(mem::kernel_table(), region);
        let wrote = getrandom_into(region, 100);
        check!(wrote == 100, "getrandom: {wrote:#x}");
        check!(
            used() == (1 + missing) * PAGE,
            "{} bytes after one page and {missing} tables",
            used()
        );
        check!(munmap(region, 16 * PAGE) == 0, "munmap");
        check!(used() == missing * PAGE, "{} still charged", used());
        Ok(())
    })
}

/// Soak: 3,000 rounds of reserving 256 MiB, touching eight scattered pages
/// and giving the range up in one of four ways (munmap, DONTNEED then munmap,
/// MAP_FIXED over it, mremap down) leave no charge but the page tables, and
/// no resident page or frame beyond them.
pub fn mmap_reservation_soak() -> Result<(), String> {
    const ROUNDS: u64 = 3000;
    quota_run(32 * MIB, || {
        let mut baseline = None;
        for round in 0..ROUNDS {
            let len = 256 * MIB;
            let region = map(len, PROT_RW, MAP_NORESERVE);
            check!(region as i64 > 0, "round {round}: mmap {region:#x}");
            for index in 0..8 {
                // One page in each of eight different 2 MiB page tables.
                let va = region + (index * 13 + round % 5) * 512 * PAGE;
                check!(touch(va), "round {round}: touch {index}");
            }
            check!(
                used() == 8 * PAGE + tables(),
                "round {round}: {} charged",
                used()
            );
            let ok = match round % 4 {
                0 => munmap(region, len) == 0,
                1 => madvise(region, len, MADV_DONTNEED) == 0 && munmap(region, len) == 0,
                2 => {
                    let fixed = mmap_fixed(region, len / 64);
                    fixed == region && munmap(region, len) == 0
                }
                _ => {
                    let down = mremap(region, len, PAGE, MREMAP_MAYMOVE, 0);
                    down == region && munmap(region, PAGE) == 0
                }
            };
            check!(ok, "round {round}: giving the range up failed");
            check!(
                used() == tables(),
                "round {round}: {} bytes charged",
                used()
            );
            // The five offset patterns each build their page tables once and
            // keep them (bounded, freed with the address space): judge growth
            // from the point every pattern has been seen.
            if round >= 20 {
                let live = mem::frame_stats().live();
                let first = *baseline.get_or_insert(live);
                check!(
                    live <= first + 8,
                    "round {round}: {} frames grew",
                    live - first
                );
            }
        }
        check!(quota::stats(UID).over_releases == 0, "released twice");
        Ok(())
    })
}

pub(in crate::tests) const CASES: &[(&str, Test)] = &[
    (
        "linux_mmap_reservation_is_charged_by_resident_pages",
        mmap_reservation_is_charged_by_resident_pages,
    ),
    (
        "linux_mmap_reservation_at_the_quota_refuses_the_touch",
        mmap_reservation_at_the_quota_refuses_the_touch,
    ),
    (
        "linux_mmap_reservation_follows_mremap",
        mmap_reservation_follows_mremap,
    ),
    (
        "linux_mmap_reservation_beside_counted_memory",
        mmap_reservation_beside_counted_memory,
    ),
    (
        "linux_mmap_reservation_is_charged_by_syscall_writes",
        mmap_reservation_is_charged_by_syscall_writes,
    ),
    ("linux_mmap_reservation_soak", mmap_reservation_soak),
];
