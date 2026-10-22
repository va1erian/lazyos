//! The AP page table and the trampoline copy (docs/smp-plan.md S8.2): the
//! one-time preparation the bring-up needs before the first SIPI, split out of
//! `smp.rs`.

use core::sync::atomic::Ordering;

use super::{trampoline, AP_PML4, CPUS, TRAMPOLINE_PHYS};
use crate::mem;

/// Build the AP page table, copy and patch the trampoline. `None` (with the
/// reason on serial) leaves the machine single-CPU.
pub(super) fn prepare() -> Option<u64> {
    // The frame allocator never hands out anything below 1 MiB, so the page
    // is the kernel's to write exactly when the boot memory map calls it
    // usable RAM; anything else (firmware, the bootloader's own data) is
    // refused rather than overwritten.
    let kind = mem::trampoline_page_kind();
    if kind != mem::LOW_PAGE_USABLE {
        serial_println!("HW:SMP:CPUS=1 (page {TRAMPOLINE_PHYS:#x} is not usable RAM, kind {kind})");
        CPUS.store(1, Ordering::Relaxed);
        return None;
    }
    let pml4 = build_ap_page_table()?;
    AP_PML4.store(pml4, Ordering::Relaxed);

    // Copy the blob and patch the one absolute value in it: the GDTR's base.
    let at = mem::phys_to_virt(x86_64::PhysAddr::new(TRAMPOLINE_PHYS)).as_u64() as *mut u8;
    let start = &raw const trampoline::smp_trampoline_start;
    let end = &raw const trampoline::smp_trampoline_end;
    let len = (end as usize - start as usize) as u64;
    if len > 4096 {
        serial_println!("HW:SMP:CPUS=1 (trampoline overflows its page)");
        CPUS.store(1, Ordering::Relaxed);
        return None;
    }
    // SAFETY: `at` is the reserved page's mapped copy, `len` its own size;
    // no other CPU is running and the source is the kernel's own text.
    unsafe {
        core::ptr::copy_nonoverlapping(start, at, len as usize);
        // The patch values: the GDTR's base, and both retf transitions'
        // (segment, offset) pairs — each target at the copy's own address.
        let at_u = at as u64;
        let blob_off = |label: &u8| (label as *const u8 as usize) - start as usize;
        let prot_off = (TRAMPOLINE_PHYS as usize + blob_off(&trampoline::smp_prot)) as u16;
        let long_off = (TRAMPOLINE_PHYS as usize + blob_off(&trampoline::smp_long_stub)) as u32;
        let put16 = |off: u32| -> *mut u16 { (at_u + off as u64) as *mut u16 };
        let put32 = |off: u32| -> *mut u32 { (at_u + off as u64) as *mut u32 };
        // The GDTR's base sits at +2 (after the `u16` limit) — unaligned by
        // construction, so the unaligned write.
        ((at_u + trampoline::GDTR_BASE_OFF as u64) as *mut u32)
            .write_unaligned(TRAMPOLINE_PHYS as u32 + trampoline::GDT_OFF);
        put16(trampoline::RETF1_CS_OFF).write_volatile(0x0008);
        put16(trampoline::RETF1_OFF_OFF).write_volatile(prot_off);
        put32(trampoline::RETF2_CS_OFF).write_volatile(0x0020);
        put32(trampoline::RETF2_OFF_OFF).write_volatile(long_off);
        // And the mailbox fields every AP shares (`CR3`, the magic); the
        // per-AP fields (`entry`, `cpu`, `stack`) are written per AP.
        ((at_u + trampoline::MB_CR3_OFF as u64) as *mut u64)
            .write_volatile(AP_PML4.load(Ordering::Relaxed));
        put32(trampoline::MB_MAGIC_OFF).write_volatile(trampoline::MB_MAGIC);
    }
    Some(TRAMPOLINE_PHYS)
}

/// Build the AP page table: identity-mapped low 2 MiB (the trampoline's
/// transitions run there) over the kernel half copied from the boot table.
/// Returns the PML4's physical address.
fn build_ap_page_table() -> Option<u64> {
    let pml4_phys = mem::frames::alloc_zeroed_frame()?.as_u64();
    let pdpt_phys = mem::frames::alloc_zeroed_frame()?.as_u64();
    let pd_phys = mem::frames::alloc_zeroed_frame()?.as_u64();
    // The trampoline loads `CR3` from a 32-bit register in protected mode,
    // and walks the PDPT/PD before long mode: all three must sit below 4 GiB.
    if pml4_phys.max(pdpt_phys).max(pd_phys) >= 1 << 32 {
        serial_println!("HW:SMP:CPUS=1 (AP page table above 4 GiB)");
        return None;
    }
    const PRESENT: u64 = 1;
    const WRITE: u64 = 1 << 1;
    const HUGE: u64 = 1 << 7;
    // PML4[0] -> PDPT[0] -> PD[0] -> the 2 MiB identity huge page 0..2 MiB.
    entry(pml4_phys, 0, pdpt_phys | PRESENT | WRITE);
    entry(pdpt_phys, 0, pd_phys | PRESENT | WRITE);
    entry(pd_phys, 0, PRESENT | WRITE | HUGE); // physical 0, the 2 MiB page
                                               // The kernel half: entries 256..512 verbatim from the boot table (the
                                               // kernel half is machine-wide; sharing the entries shares the mappings).
    let boot = mem::boot_table().as_u64();
    for index in 256..512_usize {
        let value = read_entry(boot, index)?;
        entry(pml4_phys, index, value);
    }
    Some(pml4_phys)
}

/// Write one PML4/PDPT/PD entry: `table_phys` is below 4 GiB (a freshly
/// allocated frame), so the entry write goes through the physical map.
fn entry(table_phys: u64, index: usize, value: u64) {
    let at = mem::phys_to_virt(x86_64::PhysAddr::new(table_phys + (index as u64) * 8)).as_u64()
        as *mut u64;
    // SAFETY: the frame is zeroed, exclusively ours (freshly allocated),
    // mapped through the physical map; `index` is in range.
    unsafe { at.write_volatile(value) };
}

/// Read one entry of the boot PML4 (its physical address is what
/// `mem::boot_table` reports; the entry sits in the physical map).
fn read_entry(table_phys: u64, index: usize) -> Option<u64> {
    let at = mem::phys_to_virt(x86_64::PhysAddr::new(table_phys + (index as u64) * 8)).as_u64()
        as *const u64;
    // SAFETY: the boot PML4 is machine-wide and mapped; the entry is in range.
    unsafe { Some(at.read_volatile()) }
}
