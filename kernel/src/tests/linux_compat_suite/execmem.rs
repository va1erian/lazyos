//! JIT-style executable memory (issue #773): `mmap` with `PROT_EXEC`, W^X
//! flips with `mprotect`, `mremap` and `fork` keeping the rights, the access
//! error a store to RX or a fetch from RW raises, and NX by default for the
//! stack and the heap.
//!
//! The "program" is the smallest JIT output there is, `mov eax, imm32; ret`
//! (`B8 imm32 C3`), written into a page and called through a function
//! pointer. The kernel tests run in ring 0 without SMEP, so a user-accessible
//! page is callable exactly when its PTE lacks NX, which is the bit under
//! test. A fetch that must *fault* cannot be recovered in ring 0, so those
//! cases are checked on the PTE, the page-fault decision functions and the
//! delivered `siginfo`; the ring-3 fault itself (and the stale-TLB case)
//! belongs to the `jitmem` ABI fixture.

use alloc::vec::Vec;

use super::*;
use crate::mem::vma::{Kind, Prot};
use crate::task::signal;

pub(super) const PAGE: u64 = 4096;
pub(super) const NX: u64 = 1 << 63;

const PROT_R: u64 = 1;
const PROT_W: u64 = 2;
const PROT_X: u64 = 4;
pub(super) const RW: u64 = PROT_R | PROT_W;
pub(super) const RX: u64 = PROT_R | PROT_X;
pub(super) const RWX: u64 = PROT_R | PROT_W | PROT_X;

const MAP_PRIVATE: u64 = 0x02;
const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;
pub(super) const MREMAP_MAYMOVE: u64 = 1;
pub(super) const MREMAP_FIXED: u64 = 2;

const SYS_MMAP: u64 = 9;
const SYS_MPROTECT: u64 = 10;
const SYS_MUNMAP: u64 = 11;
pub(super) const SYS_BRK: u64 = 12;
pub(super) const SYS_MREMAP: u64 = 25;

pub(super) fn mmap_prot(len: u64, prot: u64) -> Result<u64, String> {
    let addr = sys(
        SYS_MMAP,
        &[0, len, prot, MAP_PRIVATE | MAP_ANONYMOUS, u64::MAX, 0],
    );
    check!(
        addr != 0 && (addr as i64) > 0,
        "mmap(prot {prot}) returned {addr:#x}"
    );
    Ok(addr)
}

pub(super) fn mmap_fixed_prot(addr: u64, len: u64, prot: u64) -> Result<(), String> {
    let got = sys(
        SYS_MMAP,
        &[
            addr,
            len,
            prot,
            MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED,
            u64::MAX,
            0,
        ],
    );
    check!(got == addr, "MAP_FIXED mmap at {addr:#x} returned {got:#x}");
    Ok(())
}

pub(super) fn mprotect(addr: u64, len: u64, prot: u64) -> u64 {
    sys(SYS_MPROTECT, &[addr, len, prot])
}

pub(super) fn unmap(addr: u64, len: u64) -> u64 {
    sys(SYS_MUNMAP, &[addr, len])
}

/// Write `mov eax, imm; ret` at `addr`.
pub(super) fn emit(addr: u64, imm: u32) {
    let mut code = [0xB8, 0, 0, 0, 0, 0xC3];
    code[1..5].copy_from_slice(&imm.to_le_bytes());
    for (i, byte) in code.iter().enumerate() {
        // SAFETY: the caller maps `addr` writable in the kernel's user half.
        unsafe { ((addr + i as u64) as *mut u8).write_volatile(*byte) };
    }
}

/// Call the code at `addr`.
pub(super) fn call(addr: u64) -> u32 {
    // SAFETY: the caller maps `addr` executable and `emit` wrote
    // `mov eax, imm; ret` there; ring 0 may fetch from a user page when NX is
    // clear (no SMEP).
    let code: extern "C" fn() -> u32 = unsafe { core::mem::transmute(addr as usize) };
    code()
}

pub(super) fn pte(addr: u64) -> Result<u64, String> {
    raw_entry(crate::mem::kernel_table(), addr).ok_or_else(|| format!("{addr:#x} is not present"))
}

/// The PTE says: executable, writable.
pub(super) fn rights(addr: u64) -> Result<(bool, bool), String> {
    let entry = pte(addr)?;
    Ok((entry & NX == 0, entry & PTE_WRITABLE != 0))
}

pub(super) fn prot_of(addr: u64) -> Option<Prot> {
    crate::mem::vma::find(crate::mem::kernel_table(), addr).map(|vma| vma.prot)
}

/// VMA bytes and resident user pages, to compare after a test (page-table
/// frames stay behind by design, so the free-frame count is not stable).
pub(super) fn footprint() -> (u64, usize) {
    crate::mem::vma_stats(crate::mem::kernel_table())
}

/// `mmap(RW)`, write code, `mprotect(RX)`, call it, `mprotect(RW)`, patch,
/// `mprotect(RX)`, call again: every state change shows in the PTE, the VMA
/// and the page-table walk, and the page goes back to the allocator.
pub fn execmem_wx_cycle() -> Result<(), String> {
    fresh()?;
    let before = footprint();
    let page = mmap_prot(PAGE, RW)?;
    check!(prot_of(page) == Some(Prot(RW as u8)), "VMA is not RW");
    emit(page, 1);
    check!(
        rights(page)? == (false, true),
        "a fresh RW page is not NX+writable"
    );
    for n in [7u32, 0x1234_5678, 0xffff_ffff, 0] {
        emit(page, n);
        check!(mprotect(page, PAGE, RX) == 0, "mprotect(RX) failed");
        check!(prot_of(page) == Some(Prot(RX as u8)), "VMA is not RX");
        check!(
            rights(page)? == (true, false),
            "an RX page is not executable and read-only"
        );
        check!(
            crate::arch::pagewalk::user_access_allowed(page, false, true)
                && !crate::arch::pagewalk::user_access_allowed(page, true, false),
            "the walk disagrees: RX must allow fetch and refuse a store"
        );
        let got = call(page);
        check!(got == n, "the page returned {got:#x}, not {n:#x}");
        check!(mprotect(page, PAGE, RW) == 0, "mprotect(RW) failed");
        check!(
            rights(page)? == (false, true),
            "RX -> RW left the page executable or read-only"
        );
        check!(
            !crate::arch::pagewalk::user_access_allowed(page, false, true)
                && crate::arch::pagewalk::user_access_allowed(page, true, false),
            "the walk disagrees: RW must refuse a fetch and allow a store"
        );
    }
    check!(unmap(page, PAGE) == 0, "munmap failed");
    check!(footprint() == before, "the cycle leaked frames or VMAs");
    Ok(())
}

/// `PROT_READ|WRITE|EXEC` works as on Linux: written, called, patched in
/// place and called again with no `mprotect` at all.
pub fn execmem_rwx_page() -> Result<(), String> {
    fresh()?;
    let before = footprint();
    let page = mmap_prot(PAGE, RWX)?;
    emit(page, 11);
    check!(
        rights(page)? == (true, true),
        "an RWX page is not executable and writable"
    );
    check!(call(page) == 11, "RWX page: first call");
    emit(page, 12);
    check!(call(page) == 12, "RWX page: patched call");
    check!(mprotect(page, PAGE, RX) == 0, "RWX -> RX failed");
    check!(call(page) == 12, "RX page lost its code");
    check!(unmap(page, PAGE) == 0, "munmap failed");
    check!(footprint() == before, "RWX leaked");
    Ok(())
}

/// Every protection combination maps with exactly the PTE bits it names: NX
/// unless `PROT_EXEC`, writable only with `PROT_WRITE`, and nothing is
/// executable that was not asked to be.
pub fn execmem_prot_bits_map_to_pte() -> Result<(), String> {
    fresh()?;
    let table = crate::mem::kernel_table();
    let before = footprint();
    for prot in 0..8u64 {
        let page = mmap_prot(PAGE, prot)?;
        let faulted = crate::mem::demand_fault(table, page, PageFaultErrorCode::empty());
        // Only PROT_NONE has nothing to materialize.
        check!(
            faulted == (prot != 0),
            "prot {prot}: demand_fault {faulted}"
        );
        if prot != 0 {
            let exec = pte(page)? & NX == 0;
            let write = pte(page)? & PTE_WRITABLE != 0;
            check!(exec == (prot & PROT_X != 0), "prot {prot}: exec bit {exec}");
            check!(
                write == (prot & PROT_W != 0),
                "prot {prot}: write bit {write}"
            );
        }
        check!(unmap(page, PAGE) == 0, "munmap failed");
    }
    check!(footprint() == before, "the sweep leaked");
    Ok(())
}

/// The page-fault decisions for a fetch from NX and a store to RX: neither is
/// COW or demand-zero, both are `SEGV_ACCERR`; an address nothing maps is
/// `SEGV_MAPERR` whatever the access, and a hole inside a `PROT_NONE` VMA is
/// an access error again.
pub fn execmem_access_faults_are_accerr() -> Result<(), String> {
    fresh()?;
    let table = crate::mem::kernel_table();
    let page = mmap_prot(PAGE, RW)?;
    // First touch of an untouched RW page by a *fetch* only materializes a
    // non-executable page; the retried fetch then faults on NX.
    check!(
        crate::mem::demand_fault(table, page, PageFaultErrorCode::INSTRUCTION_FETCH),
        "the first fetch did not materialize the page"
    );
    check!(pte(page)? & NX != 0, "demand-zero RW memory is executable");
    let present = PageFaultErrorCode::PROTECTION_VIOLATION;
    let fetch = present | PageFaultErrorCode::USER_MODE | PageFaultErrorCode::INSTRUCTION_FETCH;
    check!(
        !crate::mem::demand_fault(table, page, fetch) && !crate::mem::cow_fault(table, page),
        "a fetch from an NX page was 'resolved'"
    );
    check!(
        signal::segv_code(fetch.bits(), true) == signal::SEGV_ACCERR,
        "fetch from NX is not SEGV_ACCERR"
    );

    emit(page, 3);
    check!(mprotect(page, PAGE, RX) == 0, "mprotect(RX) failed");
    let store = present | PageFaultErrorCode::USER_MODE | PageFaultErrorCode::CAUSED_BY_WRITE;
    check!(
        !crate::mem::demand_fault(table, page, store) && !crate::mem::cow_fault(table, page),
        "a store to an RX page was 'resolved'"
    );
    check!(
        signal::segv_code(store.bits(), true) == signal::SEGV_ACCERR,
        "store to RX is not SEGV_ACCERR"
    );
    check!(call(page) == 3, "the refused store disturbed the page");

    // Not-present faults: a write to nothing is a map error (the write bit
    // does not make it an access error), inside a PROT_NONE VMA an access error.
    let none = mmap_prot(PAGE, 0)?;
    let user = PageFaultErrorCode::USER_MODE;
    for bits in [
        user,
        user | PageFaultErrorCode::CAUSED_BY_WRITE,
        user | PageFaultErrorCode::INSTRUCTION_FETCH,
    ] {
        check!(
            signal::segv_code(bits.bits(), false) == signal::SEGV_MAPERR,
            "unmapped {bits:?} is not SEGV_MAPERR"
        );
        check!(
            !crate::mem::demand_fault(table, none, bits),
            "a PROT_NONE page was demand-mapped"
        );
        check!(
            signal::segv_code(bits.bits(), true) == signal::SEGV_ACCERR,
            "PROT_NONE {bits:?} is not SEGV_ACCERR"
        );
    }
    check!(unmap(page, PAGE) == 0 && unmap(none, PAGE) == 0, "munmap");
    Ok(())
}

/// The real delivery path: `deliver_fault` enters the `SIGSEGV` handler and
/// the `siginfo` it records carries the code and the faulting address.
pub fn execmem_sigsegv_siginfo_code() -> Result<(), String> {
    fresh()?;
    let rw = mmap_prot(PAGE, RW)?;
    let rx = mmap_prot(PAGE, RX)?;
    let none = mmap_prot(PAGE, 0)?;
    let hole = process::linux::MMAP_BASE + 0x2000_0000;
    check!(
        crate::mem::vma::find(crate::mem::kernel_table(), hole).is_none(),
        "the probe address is mapped"
    );
    let user = 0x4;
    let cases = [
        ("fetch from RW", rw + 8, 0x15u64, signal::SEGV_ACCERR),
        ("store to RX", rx + 16, 0x07, signal::SEGV_ACCERR),
        ("read of PROT_NONE", none, 0x04, signal::SEGV_ACCERR),
        ("read of a hole", hole, user, signal::SEGV_MAPERR),
        (
            "store to a hole",
            hole + 24,
            user | 0x2,
            signal::SEGV_MAPERR,
        ),
        ("fetch from a hole", hole, user | 0x10, signal::SEGV_MAPERR),
    ];
    for (what, addr, error, want) in cases {
        signal::harness::reset();
        signal::set_action(
            task::current(),
            signal::SIGSEGV,
            signal::Disposition::Handler {
                handler: 0x0040_2000,
                flags: signal::SA_SIGINFO,
                restorer: 0x0040_3000,
                mask: 0,
            },
        )
        .map_err(|e| format!("set_action: {e:?}"))?;
        let stack = UserStack::new()?;
        let mut frame = alloc::vec![0u64; signal::FAULT_RIP_INDEX + 5];
        frame[15] = error;
        frame[signal::FAULT_RIP_INDEX] = 0x0040_1234;
        frame[signal::FAULT_RIP_INDEX + 1] = 0x23;
        frame[signal::FAULT_RIP_INDEX + 2] = 0x202;
        frame[signal::FAULT_RIP_INDEX + 3] = stack.top() - 0x100;
        frame[signal::FAULT_RIP_INDEX + 4] = 0x1b;
        check!(
            signal::deliver_fault(
                frame.as_mut_ptr() as u64,
                signal::FAULT_RIP_INDEX,
                addr,
                error
            ),
            "{what}: the SIGSEGV handler was not entered"
        );
        let info = signal::harness::recorded_info(signal::SIGSEGV).ok_or("no signal state")?;
        let (code, fault_addr) = (info.code, info.addr);
        check!(code == want, "{what}: si_code {code}, want {want}");
        check!(
            fault_addr == addr,
            "{what}: si_addr {fault_addr:#x}, want {addr:#x}"
        );
    }
    signal::harness::reset();
    for page in [rw, rx, none] {
        check!(unmap(page, PAGE) == 0, "munmap failed");
    }
    Ok(())
}

/// `mprotect` over part of a VMA, or across several, splits and merges the
/// VMAs on page boundaries and sets the PTE bits of exactly those pages.
pub fn execmem_mprotect_splits_vmas() -> Result<(), String> {
    fresh()?;
    let table = crate::mem::kernel_table();
    let before = footprint();
    let base = mmap_prot(4 * PAGE, RW)?;
    for i in 0..4 {
        emit(base + i * PAGE, 100 + i as u32);
    }
    let layout = |want: &[(u64, u64, u64)]| -> Result<(), String> {
        let found = crate::mem::vma::find_range(table, base, base + 4 * PAGE);
        let got: Vec<(u64, u64, u64)> = found
            .iter()
            .map(|vma| (vma.start - base, vma.end - base, vma.prot.0 as u64))
            .collect();
        check!(got == want, "VMAs {got:x?}, want {want:x?}");
        Ok(())
    };
    let pages = |want: [(bool, bool); 4]| -> Result<(), String> {
        for (i, want) in want.iter().enumerate() {
            let got = rights(base + i as u64 * PAGE)?;
            check!(
                got == *want,
                "page {i}: (exec, write) {got:?}, want {want:?}"
            );
        }
        Ok(())
    };
    layout(&[(0, 4 * PAGE, RW)])?;

    // The middle two pages only.
    check!(mprotect(base + PAGE, 2 * PAGE, RX) == 0, "middle RX failed");
    layout(&[
        (0, PAGE, RW),
        (PAGE, 3 * PAGE, RX),
        (3 * PAGE, 4 * PAGE, RW),
    ])?;
    pages([(false, true), (true, false), (true, false), (false, true)])?;
    check!(
        call(base + PAGE) == 101 && call(base + 2 * PAGE) == 102,
        "calls"
    );

    // A range that starts in an RW VMA and ends inside the RX one.
    check!(mprotect(base, 2 * PAGE, RWX) == 0, "spanning RWX failed");
    layout(&[
        (0, 2 * PAGE, RWX),
        (2 * PAGE, 3 * PAGE, RX),
        (3 * PAGE, 4 * PAGE, RW),
    ])?;
    pages([(true, true), (true, true), (true, false), (false, true)])?;

    // Everything back to RW: the pieces merge into the original VMA.
    check!(mprotect(base, 4 * PAGE, RW) == 0, "whole RW failed");
    layout(&[(0, 4 * PAGE, RW)])?;
    pages([(false, true); 4])?;

    // A page nothing has touched takes the new rights at its first fault.
    let lazy = mmap_prot(2 * PAGE, RW)?;
    check!(mprotect(lazy + PAGE, PAGE, RX) == 0, "untouched RX failed");
    check!(
        crate::mem::demand_fault(table, lazy + PAGE, PageFaultErrorCode::INSTRUCTION_FETCH),
        "the untouched RX page did not fault in"
    );
    check!(
        rights(lazy + PAGE)? == (true, false),
        "a page faulted in after mprotect(RX) is not RX"
    );
    check!(
        unmap(lazy, 2 * PAGE) == 0 && unmap(base, 4 * PAGE) == 0,
        "munmap"
    );
    check!(footprint() == before, "splitting leaked frames or VMAs");
    Ok(())
}
