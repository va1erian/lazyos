//! A read-only software walk of the active page tables, independent of the
//! mapper and the VMA layer: it reports exactly what the paging structures
//! say, which is what a fault diagnosis has to compare the CPU (or a
//! hypervisor's emulator) against.

use x86_64::registers::control::Cr3;
use x86_64::PhysAddr;

/// Paging levels from PML4 down, with the linear-address shift of each index.
pub const LEVELS: [(&str, u64); 4] = [("pml4", 39), ("pdpt", 30), ("pd", 21), ("pt", 12)];
const PRESENT: u64 = 1;
const WRITABLE: u64 = 1 << 1;
const HUGE: u64 = 1 << 7;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;

/// The entries mapping `addr` in the active address space, PML4 first, up to
/// and including the first not-present or leaf (4 KiB or huge) entry. Returns
/// the entries and how many of them are valid.
pub fn walk(addr: u64) -> ([u64; 4], usize) {
    let mut entries = [0u64; 4];
    let mut table = Cr3::read().0.start_address().as_u64();
    for (depth, (level, shift)) in LEVELS.iter().enumerate() {
        let index = (addr >> shift) & 0x1FF;
        let entry_virt = crate::mem::phys_to_virt(PhysAddr::new(table + index * 8));
        // SAFETY: `table` is a paging structure reached from CR3 through
        // present entries, and every physical frame is mapped at the physical
        // offset; this is one aligned 8-byte load of an entry.
        let entry = unsafe { core::ptr::read_volatile(entry_virt.as_ptr::<u64>()) };
        entries[depth] = entry;
        if entry & PRESENT == 0 || is_leaf(level, entry) {
            return (entries, depth + 1);
        }
        table = entry & ADDR_MASK;
    }
    (entries, LEVELS.len())
}

/// Whether `entry` at `level` maps a page rather than naming the next table:
/// a PT entry always, a PDPT/PD entry with PS set (1 GiB / 2 MiB page). PS
/// is reserved in a PML4 entry, so it never ends a walk there.
fn is_leaf(level: &str, entry: u64) -> bool {
    level == "pt" || (matches!(level, "pdpt" | "pd") && entry & HUGE != 0)
}

/// Whether `addr` is mapped present and writable at every level, through a
/// leaf entry. Supervisor writes honour the R/W bits because the kernel sets
/// `CR0.WP`.
pub fn writable(addr: u64) -> bool {
    let (entries, count) = walk(addr);
    let (level, _) = LEVELS[count - 1];
    is_leaf(level, entries[count - 1])
        && entries[..count]
            .iter()
            .all(|entry| entry & (PRESENT | WRITABLE) == PRESENT | WRITABLE)
}
