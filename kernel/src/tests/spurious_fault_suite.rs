//! The ring-3 spurious-fault classifier (`arch::spurious_fault`).
//!
//! Under WHPX a valid user `movsb` (musl `memcpy`) occasionally came back with
//! a fabricated `#PF` without the P bit; the demand-zero path could not map
//! over the live page and the task died of SIGSEGV. The classifier asks the
//! page tables whether the access is permitted. It must say yes for exactly
//! the mapped, user-accessible, suitably-permitted pages, and no for
//! everything the real fault paths handle (unmapped, read-only write, COW,
//! supervisor pages, no-execute fetches).

use super::*;
use crate::arch::spurious_fault::is_spurious;
use x86_64::structures::paging::PageTableFlags;
use x86_64::VirtAddr;

const READ: PageFaultErrorCode = PageFaultErrorCode::USER_MODE;
const WRITE: PageFaultErrorCode =
    PageFaultErrorCode::USER_MODE.union(PageFaultErrorCode::CAUSED_BY_WRITE);
const FETCH: PageFaultErrorCode =
    PageFaultErrorCode::USER_MODE.union(PageFaultErrorCode::INSTRUCTION_FETCH);
/// What WHPX injects: reserved-bit flag only (impossible without P).
const FABRICATED: PageFaultErrorCode = PageFaultErrorCode::MALFORMED_TABLE;

/// Scratch user pages, clear of the other suites' `TEST_VA` use.
const BASE: u64 = TEST_VA + 0x0100_0000;

/// Map one fresh user page at `va` with `prot`.
fn map(va: u64, prot: Prot) -> Result<(), String> {
    let frame = mem::alloc_zeroed_frame().ok_or("out of frames")?;
    if !mem::map_page_in(
        mem::kernel_table(),
        VirtAddr::new(va),
        frame,
        mem::prot_flags(prot),
    ) {
        mem::free_frame(frame);
        return Err(format!("could not map {va:#x}"));
    }
    Ok(())
}

fn unmap(va: u64) {
    mem::unmap_range(mem::kernel_table(), va, va + 4096);
}

fn classifies_by_page_tables() -> Result<(), String> {
    let rw = BASE;
    let ro = BASE + 0x1000;
    let rx = BASE + 0x2000;
    let gone = BASE + 0x3000;
    map(rw, Prot::READ | Prot::WRITE)?;
    map(ro, Prot::READ)?;
    map(rx, Prot::READ | Prot::EXEC)?;

    let result = (|| {
        for code in [READ, FABRICATED, READ | PageFaultErrorCode::MALFORMED_TABLE] {
            check!(is_spurious(code, rw + 8), "read of rw page, {code:?}");
            check!(is_spurious(code, ro + 8), "read of ro page, {code:?}");
            check!(!is_spurious(code, gone), "read of unmapped page, {code:?}");
        }
        // A genuine reserved-bit fault (RSVD with P) is never retried.
        let real_rsvd =
            PageFaultErrorCode::MALFORMED_TABLE | PageFaultErrorCode::PROTECTION_VIOLATION;
        for code in [real_rsvd, READ | real_rsvd, WRITE | real_rsvd] {
            check!(!is_spurious(code, rw + 8), "RSVD+P fault retried, {code:?}");
        }
        check!(is_spurious(WRITE, rw + 100), "write to rw page");
        check!(!is_spurious(WRITE, ro + 100), "write to read-only page");
        check!(!is_spurious(WRITE, gone), "write to unmapped page");
        check!(!is_spurious(FETCH, rw), "fetch from no-execute page");
        check!(is_spurious(FETCH, rx), "fetch from executable page");
        check!(!is_spurious(WRITE, rx), "write to r-x page");
        // A supervisor-only page (the kernel heap) is never user-permitted.
        check!(
            !is_spurious(READ, mem::HEAP_START + 64),
            "user access to a supervisor page"
        );
        Ok(())
    })();
    for va in [rw, ro, rx] {
        unmap(va);
    }
    result?;
    check!(!is_spurious(READ, rw), "unmapped page still permitted");
    Ok(())
}

/// A present user page without WRITABLE but with the COW bit is exactly a
/// copy-on-write page: a write must go to `cow_fault`, not be retried.
fn cow_write_is_not_spurious() -> Result<(), String> {
    let va = BASE + 0x8000;
    let frame = mem::alloc_zeroed_frame().ok_or("out of frames")?;
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE
        | PageTableFlags::BIT_9;
    if !mem::map_page_in(mem::kernel_table(), VirtAddr::new(va), frame, flags) {
        mem::free_frame(frame);
        return Err("could not map the COW-shaped page".into());
    }
    let result = (|| {
        check!(!is_spurious(WRITE, va), "write to a COW page");
        check!(is_spurious(READ, va), "read of a COW page");
        Ok(())
    })();
    unmap(va);
    result
}

/// Sustained load: random map/protect/unmap over a window, the classifier
/// checked against a model after every step.
fn classifier_soak() -> Result<(), String> {
    const PAGES: u64 = 16;
    let window = BASE + 0x10_0000;
    let mut state = [None::<Prot>; PAGES as usize];
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let table = mem::kernel_table();
    let result = (|| {
        for round in 0..20_000u32 {
            let index = (next() % PAGES) as usize;
            let va = window + index as u64 * 4096;
            match next() % 3 {
                0 if state[index].is_none() => {
                    let prot = [
                        Prot::READ,
                        Prot::READ | Prot::WRITE,
                        Prot::READ | Prot::EXEC,
                    ][(next() % 3) as usize];
                    map(va, prot)?;
                    state[index] = Some(prot);
                }
                1 => {
                    unmap(va);
                    state[index] = None;
                }
                _ => {}
            }
            let page = (next() % PAGES) as usize;
            let va = window + page as u64 * 4096;
            for (code, needs) in [(READ, 0u8), (WRITE, 2), (FETCH, 4)] {
                let expected = state[page].is_some_and(|p| needs == 0 || p.0 & needs != 0);
                check!(
                    is_spurious(code, va) == expected,
                    "round {round}: page {page} state {:?} {code:?} expected {expected}",
                    state[page]
                );
            }
        }
        Ok(())
    })();
    mem::unmap_range(table, window, window + PAGES * 4096);
    result
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "spurious_fault_classifies_by_page_tables",
        classifies_by_page_tables,
    ),
    (
        "spurious_fault_cow_write_not_spurious",
        cow_write_is_not_spurious,
    ),
    ("spurious_fault_classifier_soak", classifier_soak),
];
