//! A kernel mapping of a framebuffer set up by a mode switch
//! (docs/hidpi-plan.md D1), made of 4 KiB pages so it can be write-combining.
//!
//! The firmware's framebuffer comes mapped by the bootloader with 4 KiB
//! leaves, which `wc::map_write_combining` retypes at boot. A mode switch
//! (`display::modeset`) used to reach the new, larger framebuffer through
//! the physical-memory map instead: large pages shared with all of RAM, which
//! can never be made write-combining, so a switched mode lost WC. The new
//! framebuffer now gets its own window: a free 1 GiB slot of the page-table
//! subtree that already holds the boot framebuffer. That PML4 entry is in the
//! kernel half every address space copies, so the window is visible from a
//! compositor's syscalls exactly like the boot mapping, whenever it is made.

use core::sync::atomic::{AtomicU64, Ordering};

use x86_64::structures::paging::PageTableFlags;
use x86_64::{PhysAddr, VirtAddr};

use super::{kernel_table, map_page_in, phys_to_virt, FRAME_SIZE};

/// Base of the window once chosen (0: none yet). Every switch reuses it.
static WINDOW: AtomicU64 = AtomicU64::new(0);
/// Bytes one window spans: one PDPT entry.
const SPAN: u64 = 1 << 30;
const PRESENT: u64 = 1;
const ADDR: u64 = 0x000F_FFFF_FFFF_F000;

/// Map `[phys, phys + len)` at the window (choosing it on first use, in the
/// PML4 entry holding `anchor`) and return the window's address. Pages the
/// window already maps are pointed at `phys` again, with plain write-back
/// flags; the caller applies the write-combining policy afterwards.
pub fn map(phys: u64, len: u64, anchor: u64) -> Result<u64, &'static str> {
    if !phys.is_multiple_of(FRAME_SIZE) || len == 0 {
        return Err("unaligned framebuffer");
    }
    if len > SPAN {
        return Err("framebuffer larger than the window");
    }
    let base = window(anchor)?;
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
    for page in 0..len.div_ceil(FRAME_SIZE) {
        let va = base + page * FRAME_SIZE;
        let pa = phys + page * FRAME_SIZE;
        match super::wc::leaf(va) {
            Ok(entry) => {
                // SAFETY: a present 4 KiB leaf of the window (only this module
                // maps it); it is pointed at a page of the same framebuffer.
                unsafe { entry.write_volatile(pa | flags.bits()) };
                x86_64::instructions::tlb::flush(VirtAddr::new(va));
            }
            Err(_) => {
                if !map_page_in(kernel_table(), VirtAddr::new(va), PhysAddr::new(pa), flags) {
                    return Err("no frame for a page table");
                }
            }
        }
    }
    Ok(base)
}

/// The window, choosing the highest free 1 GiB slot of `anchor`'s PML4
/// entry on first use.
fn window(anchor: u64) -> Result<u64, &'static str> {
    let chosen = WINDOW.load(Ordering::Relaxed);
    if chosen != 0 {
        return Ok(chosen);
    }
    let index = (anchor >> 39) & 0x1FF;
    if index < 256 {
        return Err("anchor outside the kernel half");
    }
    let pml4 = phys_to_virt(kernel_table()).as_ptr::<u64>();
    // SAFETY: the live kernel PML4, reached through the physical-memory map;
    // `index` is below 512.
    let entry = unsafe { pml4.add(index as usize).read_volatile() };
    if entry & PRESENT == 0 {
        return Err("anchor not mapped");
    }
    let pdpt = phys_to_virt(PhysAddr::new(entry & ADDR)).as_ptr::<u64>();
    // SAFETY: a present PML4 entry of a 4-level table names a PDPT frame.
    let free = (0..512u64)
        .rev()
        .find(|&slot| unsafe { pdpt.add(slot as usize).read_volatile() } & PRESENT == 0)
        .ok_or("no free slot beside the boot framebuffer")?;
    // Sign-extend: the kernel half's addresses have bits 48..63 set.
    let base = 0xFFFF_0000_0000_0000 | index << 39 | free << 30;
    WINDOW.store(base, Ordering::Relaxed);
    Ok(base)
}
