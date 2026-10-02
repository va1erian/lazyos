//! Write-combining for the framebuffer (H1 of `docs/real-pc-boot-plan.md`).
//!
//! The bootloader maps the framebuffer with plain 4 KiB pages (PAT entry 0,
//! write-back), so its effective memory type is whatever the firmware's MTRRs
//! say for that range, and on most PCs a GPU aperture is uncached there. An
//! uncached store is one bus transaction per pixel write: a 1080p present is
//! about two million of them, which on real hardware is the difference
//! between a usable desktop and seconds per frame. Write-combining lets the
//! CPU batch the stores into full-line bursts.
//!
//! PAT entry 7 (PTE bits PAT|PCD|PWT) is unused by the kernel: user MMIO is
//! entry 3 (PCD|PWT), everything else entry 0, and bit 7 of a 4 KiB leaf is
//! never set elsewhere. [`map_write_combining`] reprograms that entry to WC
//! and points the framebuffer's leaves at it. Per the Intel SDM, PAT WC wins
//! over an MTRR UC range, so this works whatever the firmware set. The
//! physical-memory map's alias of the same range stays as it was; nothing
//! ever touches the framebuffer through it.

use x86_64::registers::control::Cr3;
use x86_64::PhysAddr;

use super::{phys_to_virt, FRAME_SIZE};

/// `IA32_PAT`.
const IA32_PAT: u32 = 0x277;
/// The PAT memory-type encoding of write-combining.
const PAT_WC: u64 = 0x01;
/// PAT entry the framebuffer uses.
const WC_ENTRY: u32 = 7;
/// Leaf bits selecting PAT entry 7 on a 4 KiB page.
const PTE_PWT: u64 = 1 << 3;
const PTE_PCD: u64 = 1 << 4;
const PTE_PAT_4K: u64 = 1 << 7;
const PTE_PRESENT: u64 = 1;
const PTE_HUGE: u64 = 1 << 7;
const PTE_ADDR: u64 = 0x000F_FFFF_FFFF_F000;
/// Most framebuffer pages remapped (a 8K mode at 4 bytes per pixel is 32k).
const MAX_PAGES: u64 = 64 * 1024;

/// Whether the CPU has a PAT (CPUID.1:EDX bit 16).
fn has_pat() -> bool {
    core::arch::x86_64::__cpuid(1).edx & (1 << 16) != 0
}

/// The 4 KiB leaf entry mapping `va` in the active table, or why not.
fn leaf(va: u64) -> Result<*mut u64, &'static str> {
    let mut table = Cr3::read().0.start_address().as_u64();
    for shift in [39u64, 30, 21] {
        let entries = phys_to_virt(PhysAddr::new(table)).as_mut_ptr::<u64>();
        // SAFETY: `table` is a present page-table frame (the root, or named
        // by a present non-huge entry above), reachable through the
        // physical-memory map; the index is below 512.
        let entry = unsafe {
            entries
                .add(((va >> shift) & 0x1FF) as usize)
                .read_volatile()
        };
        if entry & PTE_PRESENT == 0 {
            return Err("not mapped");
        }
        if entry & PTE_HUGE != 0 {
            return Err("mapped with a large page");
        }
        table = entry & PTE_ADDR;
    }
    let entries = phys_to_virt(PhysAddr::new(table)).as_mut_ptr::<u64>();
    // SAFETY: as above, for the last level.
    let entry = unsafe { entries.add(((va >> 12) & 0x1FF) as usize) };
    // SAFETY: `entry` points into a live page-table frame.
    if unsafe { entry.read_volatile() } & PTE_PRESENT == 0 {
        return Err("not mapped");
    }
    Ok(entry)
}

/// Make `[va, va + len)` (the framebuffer) write-combining. Returns the pages
/// remapped, or why nothing was changed: every leaf is checked before any is
/// written, so a refusal leaves the mapping exactly as the bootloader made it.
pub fn map_write_combining(va: u64, len: u64) -> Result<u64, &'static str> {
    if !has_pat() {
        return Err("no PAT");
    }
    if !va.is_multiple_of(FRAME_SIZE) || len == 0 {
        return Err("unaligned framebuffer");
    }
    let pages = len.div_ceil(FRAME_SIZE);
    if pages > MAX_PAGES {
        return Err("framebuffer too large");
    }
    for page in 0..pages {
        leaf(va + page * FRAME_SIZE)?;
    }
    x86_64::instructions::interrupts::without_interrupts(|| {
        let pat = crate::arch::msr::read(IA32_PAT);
        let shift = WC_ENTRY * 8;
        let wanted = (pat & !(0xFF << shift)) | (PAT_WC << shift);
        // SAFETY: the SDM's sequence for a PAT change on one CPU: flush the
        // caches, write the MSR, flush again; no mapping used entry 7 before.
        unsafe { core::arch::asm!("wbinvd", options(nostack, preserves_flags)) };
        crate::arch::msr::write(IA32_PAT, wanted);
        for page in 0..pages {
            if let Ok(entry) = leaf(va + page * FRAME_SIZE) {
                // SAFETY: a present 4 KiB leaf of the active table (checked
                // above); only its memory-type bits change.
                unsafe {
                    entry.write_volatile(entry.read_volatile() | PTE_PAT_4K | PTE_PCD | PTE_PWT)
                };
            }
        }
        x86_64::instructions::tlb::flush_all();
        // SAFETY: as above; drops any line cached under the old type.
        unsafe { core::arch::asm!("wbinvd", options(nostack, preserves_flags)) };
    });
    Ok(pages)
}

/// Whether `va`'s leaf selects the write-combining entry and that entry is
/// programmed WC (the kernel suite checks the framebuffer with it).
#[allow(dead_code)] // Read by the kernel suite.
pub fn is_write_combining(va: u64) -> bool {
    let want = PTE_PAT_4K | PTE_PCD | PTE_PWT;
    let leaf_wc = leaf(va).is_ok_and(|entry| {
        // SAFETY: `leaf` returned a present leaf of the active table.
        let bits = unsafe { entry.read_volatile() };
        bits & want == want
    });
    let pat = crate::arch::msr::read(IA32_PAT);
    leaf_wc && (pat >> (WC_ENTRY * 8)) & 0xFF == PAT_WC
}
