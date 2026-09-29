//! Raw page-table-entry layout and access.
//!
//! Two places in the kernel walk raw x86-64 page tables by hand instead of
//! through the `x86_64` crate's safe `Mapper` abstraction, because neither
//! fits that API: `mem`'s address-space teardown/COW code walks tables that
//! are mid-teardown or carry a software COW bit the crate knows nothing
//! about, and `ipc::syscalls`'s user-pointer validator (`translate`) walks a
//! *foreign* task's table to check presence/ownership/writability before a
//! syscall touches it, materializing not-present pages along the way. Before
//! this module, `ipc::syscalls` said outright that it duplicated the PTE bit
//! layout "because this module only ever reads foreign tables" — a second,
//! independent copy of the same five constants and the same
//! cast-physical-address-to-entry-array primitive, with no way to notice if
//! they drifted apart.
//!
//! This module gives both call sites the one accessor and the one set of bit
//! layout constants. It does not validate that `phys` is actually a page
//! table (that responsibility — proving the address is a live, appropriately
//! leveled table — stays with each caller, which already has to reason about
//! that from its own walk state).

use x86_64::PhysAddr;

use crate::mem::phys_to_virt;

/// Entry is present (maps to a frame or a lower-level table).
pub const PRESENT: u64 = 1 << 0;
/// Entry allows writes.
pub const WRITABLE: u64 = 1 << 1;
/// Entry is accessible from ring 3.
pub const USER: u64 = 1 << 2;
/// Entry is a huge-page leaf (2 MiB at PD level, 1 GiB at PDPT level) rather
/// than a pointer to the next table level.
pub const HUGE: u64 = 1 << 7;
/// Mask for the physical address (frame or next-level table) an entry names.
pub const ADDR: u64 = 0x000F_FFFF_FFFF_F000;
/// Software-defined bit (bit 10, ignored by the CPU) marking a leaf that maps
/// device MMIO rather than an allocator frame (issue #240). Teardown, fork and
/// `unmap_range` skip the frame accounting for such a leaf; see `mem::mmio`.
pub const MMIO: u64 = 1 << 10;
/// No-execute bit (requires `EFER.NXE`, which `mem::init` enables at boot).
pub const NX: u64 = 1 << 63;

/// View the page table/frame at `phys` as an array of 512 raw entries.
///
/// # Safety
/// `phys` must be reachable through the kernel's physical memory map and the
/// caller's use of the returned pointer (how many entries it touches, at
/// what indices) must stay within a single page table's 512 entries.
#[inline]
pub unsafe fn table(phys: PhysAddr) -> *mut u64 {
    phys_to_virt(phys).as_mut_ptr::<u64>()
}

/// Read entry `index` (0..512) of the table at `phys`.
///
/// # Safety
/// Same contract as [`table`], plus `index` must be < 512.
#[inline]
pub unsafe fn read(phys: PhysAddr, index: usize) -> u64 {
    table(phys).add(index).read_volatile()
}
