//! `mremap`, the stack and the heap keep or lack `EXEC` as Linux says
//! (issue #773); see `execmem.rs` for the setup.

use super::execmem::*;
use super::*;
use crate::mem::vma::{Kind, Prot};

/// `mremap` of an executable region keeps `EXEC` (and the absence of write)
/// when it grows in place, moves, and is placed with `MREMAP_FIXED`; the code
/// still runs at the new address.
pub fn execmem_mremap_keeps_exec() -> Result<(), String> {
    fresh()?;
    let table = crate::mem::kernel_table();
    let before = footprint();
    let base = mmap_prot(PAGE, RW)?;
    emit(base, 0xAA);
    check!(mprotect(base, PAGE, RX) == 0, "mprotect(RX) failed");

    let grown = sys(SYS_MREMAP, &[base, PAGE, 3 * PAGE, 0, 0]);
    check!(grown == base, "in-place grow returned {grown:#x}");
    let vmas = crate::mem::vma::find_range(table, base, base + 3 * PAGE);
    check!(
        vmas.len() == 1 && vmas[0].prot == Prot(RX as u8),
        "the grown region is not one RX VMA: {vmas:?}"
    );
    check!(
        crate::mem::demand_fault(table, base + PAGE, PageFaultErrorCode::INSTRUCTION_FETCH),
        "the grown page did not fault in"
    );
    check!(
        rights(base + PAGE)? == (true, false),
        "the grown tail is not RX"
    );
    check!(call(base) == 0xAA, "the code died in the grow");

    // A blocker right above forces a move.
    mmap_fixed_prot(base + 3 * PAGE, PAGE, RW)?;
    let moved = sys(SYS_MREMAP, &[base, 3 * PAGE, 4 * PAGE, MREMAP_MAYMOVE, 0]);
    check!(
        moved != base && (moved as i64) > 0,
        "move returned {moved:#x}"
    );
    check!(
        prot_of(moved) == Some(Prot(RX as u8)) && prot_of(moved + 3 * PAGE) == Some(Prot(RX as u8)),
        "the moved VMA lost EXEC or gained write"
    );
    check!(prot_of(base).is_none(), "the old range kept its VMA");
    check!(rights(moved)? == (true, false), "the moved page is not RX");
    check!(call(moved) == 0xAA, "the code does not run after the move");

    let dest = process::linux::MMAP_BASE + 0x1000_0000;
    let fixed = sys(
        SYS_MREMAP,
        &[
            moved,
            4 * PAGE,
            4 * PAGE,
            MREMAP_MAYMOVE | MREMAP_FIXED,
            dest,
        ],
    );
    check!(fixed == dest, "MREMAP_FIXED returned {fixed:#x}");
    check!(
        prot_of(dest) == Some(Prot(RX as u8)) && rights(dest)? == (true, false),
        "the fixed move lost the rights"
    );
    check!(
        call(dest) == 0xAA,
        "the code does not run after MREMAP_FIXED"
    );

    check!(
        unmap(dest, 4 * PAGE) == 0 && unmap(base + 3 * PAGE, PAGE) == 0,
        "munmap"
    );
    check!(footprint() == before, "mremap leaked frames or VMAs");
    Ok(())
}

/// A Linux image's stack and `brk` heap are NX by default; `mprotect(EXEC)`
/// on either is honoured (Linux allows it) and takes effect on the PTE.
pub fn execmem_stack_and_heap_are_nx() -> Result<(), String> {
    fresh()?;
    let table = crate::mem::kernel_table();
    let before = footprint();
    let base = process::linux::BRK_BASE;
    check!(sys(SYS_BRK, &[0]) == base, "unexpected initial break");
    check!(
        sys(SYS_BRK, &[base + 2 * PAGE]) == base + 2 * PAGE,
        "brk grow"
    );
    let heap = crate::mem::vma::find(table, base).ok_or("no heap VMA")?;
    check!(
        heap.kind == Kind::Heap && heap.prot == Prot(RW as u8),
        "heap VMA is {heap:?}"
    );
    emit(base, 21);
    check!(
        rights(base)? == (false, true),
        "the heap page is executable"
    );
    check!(mprotect(base, PAGE, RX) == 0, "heap mprotect(RX)");
    check!(call(base) == 21, "heap code did not run after mprotect(RX)");
    check!(mprotect(base, PAGE, RW) == 0, "heap mprotect(RW)");
    check!(rights(base)? == (false, true), "heap stayed executable");
    check!(sys(SYS_BRK, &[base]) == base, "brk shrink");
    check!(footprint() == before, "the heap leaked");

    // The Linux main stack, built by the real loader.
    let elf = super::super::service_suite::minimal_elf();
    let argv = process::linux::nul_terminated(&[b"prog"]);
    let envp = process::linux::nul_terminated(&[b"A=b"]);
    let free = crate::mem::frame_stats().free;
    let user = crate::mem::new_user_table().ok_or("new_user_table failed")?;
    let outcome = (|| {
        let started = process::linux::load_image(user, &elf, &argv, &envp, (0, 0), 0)
            .map_err(|error| String::from(error.message()))?;
        let top = process::layout::STACK_TOP;
        let size = process::linux::stack_size();
        let stack = crate::mem::vma::find(user, started.rsp).ok_or("no stack VMA")?;
        check!(
            stack.kind == Kind::Stack && stack.prot == Prot(RW as u8),
            "stack VMA is {stack:?}"
        );
        let entry = raw_entry(user, started.rsp & !0xfff).ok_or("no start frame")?;
        check!(entry & NX != 0, "the eager stack page is executable");
        let deep = top - size + PAGE;
        check!(
            crate::mem::demand_fault(user, deep, PageFaultErrorCode::INSTRUCTION_FETCH),
            "a deep stack page did not fault in"
        );
        check!(
            raw_entry(user, deep).ok_or("deep page")? & NX != 0,
            "a demand-faulted stack page is executable"
        );
        // mprotect(PROT_EXEC) on the stack: honoured, as on Linux.
        let page = started.rsp & !0xfff;
        check!(
            crate::mem::protect_range(user, page, page + PAGE, Prot(RWX as u8)),
            "protect_range"
        );
        crate::mem::vma::protect(user, page, page + PAGE, Prot(RWX as u8));
        check!(
            raw_entry(user, page).ok_or("page")? & NX == 0,
            "mprotect(EXEC) on the stack was ignored"
        );
        Ok(())
    })();
    crate::mem::free_user_table(user);
    check!(
        crate::mem::frame_stats().free == free,
        "the stack check leaked frames"
    );
    outcome
}

/// `mprotect` with a zero length is a no-op that succeeds on an aligned
/// address and is `EINVAL` on an unaligned one, and a `prot` with a bit
/// beyond read, write and exec is `EINVAL` with any length. JavaScriptCore
/// reserves a growable ArrayBuffer's address range `PROT_NONE` and protects
/// the committed prefix (`OSAllocator::protect`), 0 bytes while it is empty.
pub fn execmem_mprotect_zero_length() -> Result<(), String> {
    fresh()?;
    let before = footprint();
    let base = mmap_prot(8 * PAGE, 0)?;
    check!(mprotect(base, 0, RW) == 0, "mprotect(len 0) on a reserve");
    check!(mprotect(base + PAGE, 0, RW) == 0, "mprotect(len 0) inside");
    check!(
        mprotect(base + 1, 0, RW) == EINVAL,
        "mprotect(len 0) on an unaligned address"
    );
    // Nothing changed: the reserve is still PROT_NONE, and growing it works.
    let vma = crate::mem::vma::find(crate::mem::kernel_table(), base).ok_or("no VMA")?;
    check!(vma.prot == Prot(0), "len 0 changed the protection: {vma:?}");
    check!(mprotect(base, 3 * PAGE, RW) == 0, "protect the prefix");
    check!(
        mprotect(base + 3 * PAGE, 0, RW) == 0,
        "len 0 at the prefix end"
    );
    // The same call on addresses nothing maps is still a no-op.
    check!(
        mprotect(0x7000_0000_0000, 0, RW) == 0,
        "len 0 where nothing is"
    );
    // Unknown prot bits are EINVAL first, with a zero length too, over a
    // reserve, over nothing, and without changing the protection.
    let kept = crate::mem::vma::find_range(crate::mem::kernel_table(), base, base + 8 * PAGE);
    for bad in [8, 0x10, 0x80, 1 << 31, 1 << 32, 1 << 63, u64::MAX] {
        for (addr, len) in [
            (base, 0),
            (base + PAGE, 0),
            (0x7000_0000_0000, 0),
            (base, PAGE),
            (0x7000_0000_0000, PAGE),
        ] {
            check!(
                mprotect(addr, len, bad | RW) == EINVAL,
                "mprotect({addr:#x}, {len}, {:#x}) was not EINVAL",
                bad | RW
            );
        }
    }
    check!(
        crate::mem::vma::find_range(crate::mem::kernel_table(), base, base + 8 * PAGE) == kept,
        "a refused mprotect changed the VMAs"
    );
    for round in 0..2000u64 {
        check!(
            mprotect(base + (round % 8) * PAGE, 0, round % 8) == 0,
            "round {round}: len 0"
        );
        // Any single unknown bit (3..=62), with or without the valid ones.
        let bad = (1u64 << (3 + round % 60)) | (round % 8);
        check!(
            mprotect(base + (round % 8) * PAGE, 0, bad) == EINVAL,
            "round {round}: len 0, prot {bad:#x}"
        );
        check!(
            mprotect(base + (round % 8) * PAGE, PAGE, bad) == EINVAL,
            "round {round}: len PAGE, prot {bad:#x}"
        );
    }
    check!(unmap(base, 8 * PAGE) == 0, "munmap");
    check!(footprint() == before, "the reserve leaked");
    Ok(())
}
