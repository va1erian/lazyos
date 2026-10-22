//! Address spaces seen from another CPU, the cases the read probes of
//! `smp_tlb_suite` could not reach: a span past the full-flush threshold, a
//! permission tightened under an AP's cached writable entry (a store through
//! the fault-recovering probe, `arch::probe`), and the rule that no CPU runs
//! on a page table being freed (docs/architecture/smp.md, "Dying address
//! spaces"). Vacuous with one CPU online.

use super::smp_tlb_suite::{
    acks, advanced_by, enter_and_read, leave_all, online_aps, read_all, release, tag, user_flags,
    VA,
};
use super::*;
use crate::arch::{percpu, smp};
use crate::mem::vma::Prot;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use x86_64::VirtAddr;

/// Pages between the two ends of the sparse span: past `FLUSH_ALL_ABOVE`.
const SPAN: u64 = 200;

/// Rounds of the dying-table case (`--soak`: the long one).
const TABLE_ROUNDS: u64 = if option_env!("LAZYOS_SMP_SOAK").is_some() {
    5_000
} else {
    100
};

/// The AP's store probe: write a fresh value at `va`; 1 if it faulted.
fn job_write(va: u64) -> u64 {
    // SAFETY: reads the TSC; no memory or privilege effects.
    let value = unsafe { core::arch::x86_64::_rdtsc() };
    u64::from(crate::arch::probe::write_faults(va, value))
}

/// The word at the start of `frame`.
fn word(frame: u64) -> u64 {
    let ptr = crate::mem::phys_to_virt(x86_64::PhysAddr::new(frame)).as_ptr::<u64>();
    // SAFETY: a frame this test owns, reachable through the physical map.
    unsafe { ptr.read_volatile() }
}

/// A sparse span whose ends an AP has cached, torn down by one
/// `unmap_range`: one round trip whose page count is past the full-flush
/// threshold, so the target flushes everything — and must not keep either
/// end.
fn long_sparse_span_reaches_remote_tlbs() -> Result<(), String> {
    let aps = online_aps();
    if aps.is_empty() {
        return Ok(());
    }
    let ends = [VA, VA + SPAN * 4096];
    let table = crate::mem::new_user_table().ok_or("new_user_table failed")?;
    let result = (|| {
        for (i, &va) in ends.iter().enumerate() {
            let frame = crate::mem::alloc_zeroed_frame().ok_or("no frame")?;
            tag(frame.as_u64(), 0xAB00 + i as u64);
            check!(
                crate::mem::map_page_in(table, VirtAddr::new(va), frame, user_flags()),
                "map_page_in failed"
            );
        }
        for &va in &ends {
            enter_and_read(&aps, table, va)?;
        }
        let before = acks(&aps);
        check!(
            crate::mem::unmap_range(table, VA, VA + (SPAN + 1) * 4096) == 2,
            "unmap_range did not clear both ends"
        );
        advanced_by(&aps, &before, 1, "unmap_range over a sparse 201-page span")?;
        for (i, &va) in ends.iter().enumerate() {
            let frame = crate::mem::alloc_zeroed_frame().ok_or("no frame")?;
            tag(frame.as_u64(), 0xCD00 + i as u64);
            check!(
                crate::mem::map_page_in(table, VirtAddr::new(va), frame, user_flags()),
                "remap failed"
            );
            let seen = read_all(&aps, va)?;
            check!(
                seen.iter().all(|&v| v == 0xCD00 + i as u64),
                "an AP kept a translation of the span's end {va:#x}: {seen:x?}"
            );
        }
        Ok(())
    })();
    let released = release(&aps, table);
    result.and(released)
}

/// `mprotect` to read-only under an AP that cached the page writable: its
/// next store must fault, and the frame must keep its contents. The control
/// clears the writable bit with no shootdown and logs how many APs could
/// still store through the stale entry (the premise that lets this fail).
fn protect_range_revokes_remote_writes() -> Result<(), String> {
    let aps = online_aps();
    if aps.is_empty() {
        return Ok(());
    }
    let table = crate::mem::new_user_table().ok_or("new_user_table failed")?;
    let result = (|| {
        let frame = crate::mem::alloc_zeroed_frame().ok_or("no frame")?;
        check!(
            crate::mem::map_page_in(table, VirtAddr::new(VA), frame, user_flags()),
            "map_page_in failed"
        );
        enter_and_read(&aps, table, VA)?;
        let stores = |what: &str| -> Result<Vec<bool>, String> {
            aps.iter()
                .map(|&ap| {
                    smp::run_on(ap, job_write, VA)
                        .map(|faulted| faulted == 0)
                        .ok_or_else(|| format!("an AP never ran the store ({what})"))
                })
                .collect()
        };
        check!(
            stores("writable")?.iter().all(|&s| s),
            "an AP could not store to a writable page"
        );
        // The control: the PTE loses its writable bit behind the TLB's back.
        let entry = crate::mem::leaf_pte_for_test(table, VA).ok_or("no leaf")?;
        // SAFETY: a leaf of the scratch table, which only this test uses.
        unsafe { entry.write_volatile(entry.read_volatile() & !crate::mem::pte::WRITABLE) };
        let stale = stores("control")?.iter().filter(|&&s| s).count();
        serial_println!(
            "TEST:smp_tlb control: {stale}/{} APs stored through a stale writable entry",
            aps.len()
        );
        // SAFETY: as above; writable again, flushed everywhere, re-cached.
        unsafe { entry.write_volatile(entry.read_volatile() | crate::mem::pte::WRITABLE) };
        crate::mem::tlb::shootdown(VA, 1);
        check!(
            stores("re-armed")?.iter().all(|&s| s),
            "an AP could not store after the writable bit came back"
        );
        let kept = word(frame.as_u64());
        check!(
            crate::mem::protect_range(table, VA, VA + 4096, Prot::READ),
            "protect_range refused"
        );
        let after = stores("read-only")?;
        check!(
            after.iter().all(|&s| !s) && word(frame.as_u64()) == kept,
            "an AP stored through its stale writable entry after protect_range: {after:?}"
        );
        Ok(())
    })();
    let released = release(&aps, table);
    result.and(released)
}

/// Idle APs sit on the boot table; `free_user_table`'s check sees exactly
/// the CPUs on a table, and none once they left. Then the same round trip
/// many times over fresh tables, freed each time: no frame leaks.
fn dying_tables_have_no_cpu_on_them() -> Result<(), String> {
    let aps = online_aps();
    if aps.is_empty() {
        return Ok(());
    }
    let boot = crate::mem::boot_table().as_u64();
    for &ap in &aps {
        let cr3 = percpu::area(ap).map_or(0, |c| c.cr3.load(Ordering::Acquire));
        check!(
            cr3 == boot,
            "idle AP {ap} sits on {cr3:#x}, not the boot table"
        );
    }
    let mask = aps.iter().fold(0u64, |m, &ap| m | 1 << ap);
    // Round 0 warms the heap (a heap that grows takes frames for good);
    // the leak check counts from round 1.
    let mut free_before = 0;
    for round in 0..=TABLE_ROUNDS {
        if round == 1 {
            free_before = crate::mem::frame_stats().free;
        }
        let table = crate::mem::new_user_table().ok_or("new_user_table failed")?;
        crate::process::map_range(table, VA, VA + 4096)?;
        enter_and_read(&aps, table, VA)?;
        let on = smp::others_on_table(table.as_u64());
        let left = leave_all(&aps);
        let after = smp::others_on_table(table.as_u64());
        check!(
            left,
            "round {round}: an AP did not leave the table (kept, not freed)"
        );
        crate::mem::free_user_table(table);
        check!(
            on == mask && after == 0,
            "round {round}: CPUs on the table {on:#x} (expected {mask:#x}), after leaving {after:#x}"
        );
    }
    let free_after = crate::mem::frame_stats().free;
    check!(
        free_after == free_before,
        "{TABLE_ROUNDS} tables: {free_before} free frames before, {free_after} after"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "smp_tlb_long_sparse_span_reaches_remote_tlbs",
        long_sparse_span_reaches_remote_tlbs,
    ),
    (
        "smp_tlb_protect_range_revokes_remote_writes",
        protect_range_revokes_remote_writes,
    ),
    (
        "smp_dying_tables_have_no_cpu_on_them",
        dying_tables_have_no_cpu_on_them,
    ),
];
