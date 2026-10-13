//! A reservation (`PROT_NONE`, `MAP_NORESERVE`) is charged to the user-memory
//! quota by the pages that come to be resident, not by its length: a runtime
//! that sets aside gigabytes of address space and fills it in pieces pays for
//! the pieces (WebKit's 1 GiB code pool and 8 GiB structure heap were
//! refusing a second page process under the 8 GiB quota). The charge is taken
//! at the first touch, given back as the pages are unmapped, dropped or moved,
//! and a uid at its quota is refused the touch, not the reservation.

use super::*;
use crate::quota;
use x86_64::structures::idt::PageFaultErrorCode;

const PROT_NONE: u64 = 0;
const MAP_NORESERVE: u64 = 0x4000;
const MADV_DONTNEED: u64 = 4;
const GIB: u64 = 1 << 30;
const ENOMEM: u64 = (-12i64) as u64;

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
    mem::demand_fault(mem::kernel_table(), va, PageFaultErrorCode::CAUSED_BY_WRITE)
}

fn resident(start: u64, len: u64) -> u64 {
    mem::resident_pages(mem::kernel_table(), start, start + len)
}

/// Gigabytes of reservation cost nothing under a 64 MiB quota; each page
/// touched costs one page; `madvise(DONTNEED)`, `munmap` and a `MAP_FIXED`
/// replacement give back exactly the pages that were resident, and a
/// reservation that was made read-write by `mprotect` stays charged by page.
pub fn mmap_reservation_is_charged_by_resident_pages() -> Result<(), String> {
    with_quota(64 * MIB, || {
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
        check!(used() == 16 * PAGE, "{} bytes charged for 16 pages", used());
        check!(
            resident(region, GIB) == 16,
            "{} resident",
            resident(region, GIB)
        );

        // PROT_NONE: nothing to touch, nothing charged; `mprotect` commits
        // nothing either, the pages are charged as they are touched.
        check!(!touch(none), "a PROT_NONE page came to be");
        check!(mprotect(none, 64 * PAGE, PROT_RW) == 0, "mprotect");
        check!(used() == 16 * PAGE, "mprotect charged: {}", used());
        for index in 0..4 {
            check!(touch(none + index * PAGE), "touch {index} after mprotect");
        }
        check!(used() == 20 * PAGE, "{} bytes after 4 more", used());
        check!(
            !touch(none + 100 * PAGE),
            "a page still PROT_NONE came to be"
        );
        check!(used() == 20 * PAGE, "a refused touch charged: {}", used());

        // MADV_DONTNEED drops the pages and their charge, not the range.
        check!(madvise(none, 64 * PAGE, MADV_DONTNEED) == 0, "madvise");
        check!(used() == 16 * PAGE, "{} bytes after DONTNEED", used());
        check!(touch(none), "a dropped page did not fault back in");
        check!(used() == 17 * PAGE, "{} bytes after the refault", used());

        // munmap of the first 24 pages: 8 of the 16 touched pages lived there.
        check!(munmap(region, 24 * PAGE) == 0, "munmap");
        check!(used() == 9 * PAGE, "{} bytes after munmap", used());

        // MAP_FIXED over the other 8: the new mapping is charged by length and
        // the replaced reservation gives back its 8 resident pages.
        let fixed = mmap_fixed(region + 24 * PAGE, 32 * PAGE);
        check!(fixed == region + 24 * PAGE, "MAP_FIXED gave {fixed:#x}");
        check!(
            used() == (1 + 32) * PAGE,
            "{} bytes after MAP_FIXED",
            used()
        );
        check!(munmap(fixed, 32 * PAGE) == 0, "munmap of the fixed mapping");
        check!(used() == PAGE, "{} bytes, one page left", used());

        check!(munmap(none, GIB) == 0, "munmap of the PROT_NONE range");
        check!(
            munmap(region + 56 * PAGE, GIB - 56 * PAGE) == 0,
            "munmap of the tail"
        );
        check!(used() == 0, "{} bytes still charged", used());
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
    with_quota(8 * PAGE, || {
        let region = map(GIB, PROT_RW, MAP_NORESERVE);
        check!(
            region as i64 > 0,
            "reservation under a tiny quota: {region:#x}"
        );
        for index in 0..8 {
            check!(
                touch(region + index * PAGE),
                "touch {index} within the quota"
            );
        }
        check!(used() == 8 * PAGE, "{} bytes charged", used());
        let frames = mem::frame_stats().live();
        check!(!touch(region + 8 * PAGE), "a ninth page came to be");
        check!(used() == 8 * PAGE, "the refusal charged: {}", used());
        check!(resident(region, GIB) == 8, "the refused page is mapped");
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

        check!(munmap(region, 4 * PAGE) == 0, "munmap");
        check!(used() == 4 * PAGE, "{} bytes after munmap", used());
        check!(touch(region + 8 * PAGE), "no room after munmap");
        check!(
            munmap(region, GIB) == 0 && munmap(more, GIB) == 0,
            "cleanup"
        );
        check!(used() == 0, "{} bytes still charged", used());
        check!(quota::stats(UID).over_releases == 0, "released twice");
        Ok(())
    })
}

/// `mremap` keeps a reservation's charge with its pages: growing or moving
/// costs nothing, shrinking gives back the resident pages of the tail.
pub fn mmap_reservation_follows_mremap() -> Result<(), String> {
    with_quota(64 * MIB, || {
        let region = map(64 * PAGE, PROT_RW, MAP_NORESERVE);
        check!(region as i64 > 0, "mmap: {region:#x}");
        for index in 0..8 {
            check!(touch(region + index * PAGE), "touch {index}");
        }
        let moved = mremap(region, 64 * PAGE, 4096 * PAGE, MREMAP_MAYMOVE, 0);
        check!(moved as i64 > 0, "mremap grow: {moved:#x}");
        check!(used() == 8 * PAGE, "{} bytes after growing", used());
        check!(resident(moved, 4096 * PAGE) == 8, "pages did not move");

        let shrunk = mremap(moved, 4096 * PAGE, 4 * PAGE, MREMAP_MAYMOVE, 0);
        check!(shrunk == moved, "mremap shrink: {shrunk:#x}");
        check!(used() == 4 * PAGE, "{} bytes after shrinking", used());

        check!(munmap(moved, 4 * PAGE) == 0, "munmap");
        check!(used() == 0, "{} bytes still charged", used());
        check!(quota::stats(UID).over_releases == 0, "released twice");
        Ok(())
    })
}

/// Soak: 3,000 rounds of reserving 256 MiB, touching eight scattered pages
/// and giving the range up in one of four ways (munmap, DONTNEED then munmap,
/// MAP_FIXED over it, mremap down) leave no charge, resident page or frame.
pub fn mmap_reservation_soak() -> Result<(), String> {
    const ROUNDS: u64 = 3000;
    with_quota(32 * MIB, || {
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
            check!(used() == 8 * PAGE, "round {round}: {} charged", used());
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
            check!(used() == 0, "round {round}: {} bytes charged", used());
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
