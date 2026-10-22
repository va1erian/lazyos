//! TLB shootdown, proven from the other side (docs/smp-plan.md S8.2): an AP
//! caches a translation, the BSP changes the mapping through the real kernel
//! paths (`unmap_range`, the COW copy, fork, `mprotect`), and the AP reads
//! again. A read that still returns the old frame's contents is a stale
//! entry, which on a running system is a store into someone else's page.
//!
//! The AP runs the probes as jobs in its reschedule IPI (`smp::run_on`); it
//! switches to the scratch table with one job and stays on it, because a CR3
//! write would flush the very entry under test. Every test puts the AP back
//! on the boot table before the scratch table is freed. All cases are
//! vacuous with one CPU online (`-smp 1`), like the rest of the `smp_` set.

use super::*;
use crate::arch::percpu;
use crate::arch::smp;
use crate::mem::vma::Prot;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use x86_64::VirtAddr;

/// Where the scratch pages live in the scratch tables.
const VA: u64 = 0x0000_0010_0000_0000;

fn online_aps() -> Vec<usize> {
    (1..percpu::MAX_CPUS)
        .filter(|&i| percpu::area(i).is_some_and(|cpu| cpu.online.load(Ordering::Acquire)))
        .collect()
}

/// The AP's jobs. They run inside the IPI handler: no lock, no allocation,
/// and every address they touch is mapped when they run.
fn job_enter(table: u64) -> u64 {
    crate::mem::switch_to(x86_64::PhysAddr::new(table));
    0
}

fn job_leave(_: u64) -> u64 {
    crate::mem::switch_to(crate::mem::boot_table());
    0
}

fn job_read(va: u64) -> u64 {
    // SAFETY: the BSP maps `va` in the table this AP runs on before asking.
    unsafe { (va as *const u64).read_volatile() }
}

/// Put a distinguishing word at the start of `frame`.
fn tag(frame: u64, value: u64) {
    let ptr = crate::mem::phys_to_virt(x86_64::PhysAddr::new(frame)).as_mut_ptr::<u64>();
    // SAFETY: a frame this test owns, reachable through the physical map.
    unsafe { ptr.write_volatile(value) };
}

fn user_flags() -> x86_64::structures::paging::PageTableFlags {
    crate::mem::prot_flags(Prot::READ | Prot::WRITE)
}

fn pte(table: x86_64::PhysAddr, va: u64) -> Result<u64, String> {
    raw_entry(table, va).ok_or_else(|| format!("{va:#x} is not mapped"))
}

/// Every online AP runs on `table` and has read `va` (so holds its
/// translation); returns what they read.
fn enter_and_read(aps: &[usize], table: x86_64::PhysAddr, va: u64) -> Result<Vec<u64>, String> {
    let mut seen = Vec::new();
    for &ap in aps {
        smp::run_on(ap, job_enter, table.as_u64()).ok_or("an AP never ran the enter job")?;
        seen.push(smp::run_on(ap, job_read, va).ok_or("an AP never ran the read job")?);
    }
    Ok(seen)
}

fn read_all(aps: &[usize], va: u64) -> Result<Vec<u64>, String> {
    aps.iter()
        .map(|&ap| smp::run_on(ap, job_read, va).ok_or_else(|| "an AP never read".into()))
        .collect()
}

/// Put every AP back on the boot table. An AP that does not answer is still
/// on the scratch table, which must then not be freed: the caller leaks it.
fn leave_all(aps: &[usize]) -> bool {
    aps.iter().fold(true, |ok, &ap| {
        smp::run_on(ap, job_leave, 0).is_some() && ok
    })
}

/// [`leave_all`] and then free `table`, or keep it (and say so) when an AP
/// could not be taken off it.
fn release(aps: &[usize], table: x86_64::PhysAddr) -> Result<(), String> {
    if leave_all(aps) {
        crate::mem::free_user_table(table);
        Ok(())
    } else {
        Err("an AP did not leave the scratch table; it is kept, not freed".into())
    }
}

fn acks(aps: &[usize]) -> Vec<u64> {
    aps.iter()
        .filter_map(|&ap| percpu::area(ap))
        .map(|cpu| cpu.ipi.sd_ack.load(Ordering::Acquire))
        .collect()
}

/// Each AP acknowledged exactly `by` more shootdowns than at `before`.
fn advanced_by(aps: &[usize], before: &[u64], by: u64, what: &str) -> Result<(), String> {
    let now = acks(aps);
    check!(
        now.iter()
            .zip(before)
            .all(|(n, b)| n.wrapping_sub(*b) == by),
        "{what}: expected {by} shootdown(s) per AP, got {:?} -> {now:?}",
        before
    );
    Ok(())
}

/// `unmap_range` clears a page an AP has cached and frees its frame: the AP
/// must not see the old frame afterwards, even though the same address now
/// maps another one.
fn unmap_range_reaches_remote_tlbs() -> Result<(), String> {
    let aps = online_aps();
    if aps.is_empty() {
        return Ok(());
    }
    let table = crate::mem::new_user_table().ok_or("new_user_table failed")?;
    let result = (|| {
        let old = crate::mem::alloc_zeroed_frame().ok_or("no frame")?;
        let new = crate::mem::alloc_zeroed_frame().ok_or("no frame")?;
        tag(old.as_u64(), 0xA1A1_A1A1_A1A1_A1A1);
        tag(new.as_u64(), 0xC3C3_C3C3_C3C3_C3C3);
        check!(
            crate::mem::map_page_in(table, VirtAddr::new(VA), old, user_flags()),
            "map_page_in failed"
        );
        let first = enter_and_read(&aps, table, VA)?;
        check!(
            first.iter().all(|&v| v == 0xA1A1_A1A1_A1A1_A1A1),
            "the AP did not see the first frame: {first:x?}"
        );
        check!(
            crate::mem::unmap_range(table, VA, VA + 4096) == 1,
            "unmap_range did not clear the page"
        );
        check!(
            crate::mem::map_page_in(table, VirtAddr::new(VA), new, user_flags()),
            "remap failed"
        );
        let second = read_all(&aps, VA)?;
        check!(
            second.iter().all(|&v| v == 0xC3C3_C3C3_C3C3_C3C3),
            "an AP read through a stale translation after unmap_range: {second:x?}"
        );
        Ok(())
    })();
    let released = release(&aps, table);
    result.and(released)
}

/// The control for the test above: with the PTE changed behind the TLB's
/// back, the AP *does* keep reading the old frame on real TLBs. Not a
/// failure when the host's TLB is looser (a hypervisor may drop entries),
/// but a log line that the premise held is what makes the case above able
/// to fail.
fn stale_entries_exist_without_a_shootdown() -> Result<(), String> {
    let aps = online_aps();
    if aps.is_empty() {
        return Ok(());
    }
    let table = crate::mem::new_user_table().ok_or("new_user_table failed")?;
    let result = (|| {
        let old = crate::mem::alloc_zeroed_frame().ok_or("no frame")?;
        let new = crate::mem::alloc_zeroed_frame().ok_or("no frame")?;
        tag(old.as_u64(), 0x0101_0101_0101_0101);
        tag(new.as_u64(), 0x0202_0202_0202_0202);
        check!(
            crate::mem::map_page_in(table, VirtAddr::new(VA), old, user_flags()),
            "map_page_in failed"
        );
        enter_and_read(&aps, table, VA)?;
        let entry = crate::mem::leaf_pte_for_test(table, VA).ok_or("no leaf")?;
        // SAFETY: a leaf of the scratch table, which only this test uses.
        unsafe {
            let flags = entry.read_volatile() & !crate::mem::pte::ADDR;
            entry.write_volatile(new.as_u64() | flags);
        }
        let stale = read_all(&aps, VA)?
            .iter()
            .filter(|&&v| v == 0x0101_0101_0101_0101)
            .count();
        serial_println!(
            "TEST:smp_tlb control: {stale}/{} APs kept the stale translation",
            aps.len()
        );
        // And each way of shooting down cures it: a page, "everything" (the
        // 0/0 wire case), and a range long enough that the target flushes
        // everything instead of walking it.
        let cures: [(&str, fn()); 3] = [
            ("one page", || crate::mem::tlb::shootdown(VA, 1)),
            ("everything", crate::mem::tlb::shootdown_all),
            ("a long range", || crate::mem::tlb::shootdown(VA, 100)),
        ];
        let mut current = new;
        for (round, (name, cure)) in cures.iter().enumerate() {
            let fresh_frame = crate::mem::alloc_zeroed_frame().ok_or("no frame")?;
            let mark = 0x0300_0000_0000_0000 + round as u64;
            tag(fresh_frame.as_u64(), mark);
            if round > 0 {
                // Warm the translation again, then change it behind its back.
                read_all(&aps, VA)?;
                let entry = crate::mem::leaf_pte_for_test(table, VA).ok_or("no leaf")?;
                // SAFETY: as above.
                unsafe {
                    let flags = entry.read_volatile() & !crate::mem::pte::ADDR;
                    entry.write_volatile(fresh_frame.as_u64() | flags);
                }
            }
            cure();
            let seen = read_all(&aps, VA)?;
            let want = if round > 0 {
                mark
            } else {
                0x0202_0202_0202_0202
            };
            check!(
                seen.iter().all(|&v| v == want),
                "shooting down {name} left a stale translation: {seen:x?}"
            );
            if round > 0 {
                crate::mem::free_frame(current);
                current = fresh_frame;
            } else {
                crate::mem::free_frame(fresh_frame);
            }
        }
        let _ = current; // owned by the scratch table now: freed with it
        crate::mem::free_frame(old);
        Ok(())
    })();
    let released = release(&aps, table);
    result.and(released)
}

/// The COW *copy* drops the faulting space's reference to the shared frame:
/// a sibling CPU still reading that frame through a stale read-only entry
/// would see its other owner's writes (or a freed frame). The AP must see
/// the private copy.
fn cow_copy_reaches_remote_tlbs() -> Result<(), String> {
    let aps = online_aps();
    if aps.is_empty() {
        return Ok(());
    }
    let parent = crate::mem::new_user_table().ok_or("new_user_table failed")?;
    let mut child = None;
    let result = (|| {
        let pages = crate::process::map_range(parent, VA, VA + 4096)?;
        tag(pages[0].1, 0xF0F0_F0F0_F0F0_F0F0);
        let before = acks(&aps);
        child = Some(crate::mem::clone_user_table(parent).ok_or("clone_user_table failed")?);
        advanced_by(&aps, &before, 1, "fork")?;
        let seen = enter_and_read(&aps, parent, VA)?;
        check!(
            seen.iter().all(|&v| v == 0xF0F0_F0F0_F0F0_F0F0),
            "the AP did not see the shared frame: {seen:x?}"
        );
        let before = acks(&aps);
        check!(crate::mem::cow_fault(parent, VA), "cow_fault failed");
        advanced_by(&aps, &before, 1, "the COW copy")?;
        let copy = pte(parent, VA)? & crate::mem::pte::ADDR;
        check!(copy != pages[0].1, "the page was not copied");
        tag(copy, 0x5A5A_5A5A_5A5A_5A5A);
        let after = read_all(&aps, VA)?;
        check!(
            after.iter().all(|&v| v == 0x5A5A_5A5A_5A5A_5A5A),
            "an AP read the shared frame through a stale entry after the COW copy: {after:x?}"
        );
        Ok(())
    })();
    let released = release(&aps, parent);
    if let Some(child) = child {
        crate::mem::free_user_table(child);
    }
    result.and(released)
}

/// The paths whose effect cannot be probed with a read (a permission, a
/// write-protect) are held to the other observable: every AP acknowledged
/// the shootdown, once per batch and not once per page.
fn batches_cost_one_round_trip_per_chunk() -> Result<(), String> {
    let aps = online_aps();
    if aps.is_empty() {
        return Ok(());
    }
    let table = crate::mem::new_user_table().ok_or("new_user_table failed")?;
    let result = (|| {
        const PAGES: u64 = 40;
        crate::process::map_range(table, VA, VA + PAGES * 4096)?;
        let before = acks(&aps);
        check!(
            crate::mem::protect_range(table, VA, VA + PAGES * 4096, Prot::READ),
            "protect_range refused"
        );
        advanced_by(&aps, &before, 1, "protect_range over 40 pages")?;
        // A range that holds nothing changes nothing and interrupts no one.
        let before = acks(&aps);
        crate::mem::protect_range(table, VA + 0x100_0000, VA + 0x100_1000, Prot::READ);
        advanced_by(&aps, &before, 0, "protect_range over nothing")?;
        // unmap_range frees 32 frames per round trip: 40 pages are two.
        let before = acks(&aps);
        check!(
            crate::mem::unmap_range(table, VA, VA + PAGES * 4096) == PAGES as usize,
            "unmap_range cleared the wrong count"
        );
        advanced_by(&aps, &before, 2, "unmap_range over 40 pages")?;
        // An empty batch costs nothing.
        let before = acks(&aps);
        crate::mem::tlb::Batch::new().finish();
        advanced_by(&aps, &before, 0, "an empty batch")?;
        Ok(())
    })();
    crate::mem::free_user_table(table);
    result
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "smp_tlb_unmap_range_reaches_remote_tlbs",
        unmap_range_reaches_remote_tlbs,
    ),
    (
        "smp_tlb_stale_entries_exist_without_a_shootdown",
        stale_entries_exist_without_a_shootdown,
    ),
    (
        "smp_tlb_cow_copy_reaches_remote_tlbs",
        cow_copy_reaches_remote_tlbs,
    ),
    (
        "smp_tlb_batches_cost_one_round_trip_per_chunk",
        batches_cost_one_round_trip_per_chunk,
    ),
];
