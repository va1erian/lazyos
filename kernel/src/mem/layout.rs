//! The virtual address-space layout every address space shares.
//!
//! ```text
//! 0x0000_0000_0000_0000 ┐
//!                       │ private user window: PML4 entries 0..=254 (~127.5 TiB),
//!                       │ owned by each address space (fork copies it, teardown
//!                       │ frees it); the per-ABI layout inside it is in
//!                       │ `process::linux::layout` and `process` (native)
//! 0x0000_7f80_0000_0000 ┤ shared-buffer window: PML4 entry 255 (512 GiB), the
//!                       │ same entry in every table (`ipc::shared_va`)
//! 0x0000_8000_0000_0000 ┘ end of the canonical lower half
//! 0xffff_8000_0000_0000 ┐ bootloader mappings: kernel image, boot stack, boot
//!                       │ info, framebuffer, physical-memory map (entries 256..=383)
//! 0xffff_c000_0000_0000 ┤ kernel heap (entry 384, `mem::heap`), grown on demand
//! 0xffff_c080_0000_0000 ┘ unused kernel half
//! ```
//!
//! Only the private window differs between address spaces. `new_user_table`
//! copies every entry from [`USER_PML4_ENTRIES`] up from the kernel's table, so
//! the kernel half (and the shared window) must have its top-level entries
//! before the first address space is created, and the kernel must keep nothing
//! it needs inside the private window: [`check_kernel_table`] verifies that at
//! boot.

use x86_64::PhysAddr;

use crate::error::{kstop, KError};

use super::pte;

/// PML4 entries `0..USER_PML4_ENTRIES` are each address space's own.
pub const USER_PML4_ENTRIES: usize = 255;
/// One past the highest private user address.
pub const USER_TOP: u64 = (USER_PML4_ENTRIES as u64) << 39;
/// Base of the shared-buffer window (PML4 entry 255, up to the end of the
/// canonical lower half at `1 << 47`).
pub const SHARED_WINDOW_BASE: u64 = USER_TOP;
/// Lowest address the bootloader may place its dynamic mappings at.
pub const BOOT_DYNAMIC_START: u64 = 0xffff_8000_0000_0000;
/// Highest address the bootloader may place its dynamic mappings at (below
/// the heap's PML4 entry).
pub const BOOT_DYNAMIC_END: u64 = super::HEAP_START - 0x1000;

/// Stop the boot if the kernel's own table maps anything in PML4 entries
/// `1..=255`: an address space would not inherit it (entry 0 holds the
/// bootloader's identity-mapped hand-off code, which the kernel never uses
/// again). A mapping there means the bootloader ignored the dynamic range.
pub(super) fn check_kernel_table(kernel: PhysAddr) {
    for index in 1..=USER_PML4_ENTRIES {
        // SAFETY: `kernel` is the live boot PML4, reachable through the
        // physical-memory map, and `index` is below 512.
        let entry = unsafe { pte::read(kernel, index) };
        if entry & pte::PRESENT != 0 {
            crate::serial_println!("mem: kernel PML4 entry {index} is in use: {entry:#x}");
            kstop(KError::Io, "kernel mapping inside the user half");
        }
    }
}
