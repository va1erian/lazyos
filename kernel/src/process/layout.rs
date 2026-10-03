//! The user address-space layout of a process, inside the private window
//! `mem::layout` gives every address space (`0 .. USER_TOP`, ~127.5 TiB).
//!
//! ```text
//! 0x0000_0000_0000 ┐ the ELF image at its link address (static-PIE: 0,
//!                  │ classic static: 0x40_0000), then the heap: Linux `brk`
//!                  │ starts at the page after the image, native `sbrk` at
//!                  │ USER_HEAP_BASE or after the image, both up to MMAP_BASE
//! 0x1000_0000_0000 ┤ MMAP_BASE: anonymous `mmap`, first fit (96 TiB)
//! 0x7000_0000_0000 ┤ MMAP_LIMIT = MMIO_BASE: device BAR mappings (`dev::ops`)
//! 0x7008_0000_0000 ┤ MMIO_END
//!        ...       │ (unused)
//!   STACK_TOP - s  │ main-thread stack, `s` = `limit.stack_size` (8 MiB by
//! 0x7f00_0000_0000 ┤ default), demand-zero below the start frame
//!        ...       │ 512 GiB unmapped guard
//! 0x7f80_0000_0000 ┘ USER_TOP: the shared-buffer window starts here
//! ```
//!
//! Every region is far larger than any machine LazyOS targets can back, so
//! the practical limits are RAM and the per-uid quota, not the layout. An
//! image must end below [`MMAP_BASE`] ([`IMAGE_RESERVED`]): everything above
//! is placed by the kernel.

use crate::mem::USER_TOP;

/// Start of the anonymous `mmap` area; also the ceiling of `brk`/`sbrk` and
/// of the loaded image.
pub const MMAP_BASE: u64 = 0x1000_0000_0000;
/// End of the anonymous `mmap` area.
pub const MMAP_LIMIT: u64 = 0x7000_0000_0000;
/// Device MMIO mappings (`dev::ops::map_bar`), private to the claimant.
pub const MMIO_BASE: u64 = MMAP_LIMIT;
/// End of the MMIO range (exclusive): 32 GiB of address space.
pub const MMIO_END: u64 = MMIO_BASE + (32 << 30);
/// Top of every main-thread stack (Linux and native); stacks grow down.
pub const STACK_TOP: u64 = 0x7f00_0000_0000;
/// Largest stack the layout leaves room for below [`STACK_TOP`]; the
/// configured size (`limit.stack_size`) is clamped to it.
pub const STACK_MAX: u64 = STACK_TOP - MMIO_END;

/// Windows an ELF image may not occupy: everything the kernel places itself.
pub const IMAGE_RESERVED: [(u64, u64); 1] = [(MMAP_BASE, USER_TOP)];

const _: () = assert!(STACK_TOP < USER_TOP && MMIO_END < STACK_TOP);
const _: () = assert!(STACK_MAX >= 1 << 30, "the 1 GiB stack ceiling must fit");

/// The page-aligned end of the highest segment, the first address a heap
/// (`brk`/`sbrk`) may use; `floor` for an image that ends below it.
pub fn heap_start(image_end: u64, floor: u64) -> u64 {
    let aligned = image_end.saturating_add(0xfff) & !0xfff;
    aligned.max(floor).min(MMAP_BASE)
}
