//! Per-address-space memory accounting: resident, private and proportional
//! sizes that count shared pages the way a user expects.
//!
//! A frame's reference count is not the number of address spaces that map it:
//! the program-image page cache and a memfd each hold a reference of their own,
//! and the shared-buffer window's leaves hold none (the buffer registry does).
//! Dividing a frame between its mappers therefore needs the number of
//! *mappings*, which [`measure`] counts in one walk over the address spaces it
//! is given:
//!
//! * a leaf whose frame has one reference is private to its space (nothing else
//!   can map it), tallied at once;
//! * every other leaf is remembered with its frame and tallied once the walk
//!   has seen all the spaces, when the frame's mappings are known.
//!
//! A page mapped by `n` spaces adds one page to each space's resident size and
//! `1 / n` of a page to its proportional size (PSS), so the proportional sizes
//! of all spaces add up to the memory the programs really hold: the program
//! text of a binary that ten processes run is counted once, not ten times. A
//! page only one space maps is *private* (USS) even when the page cache also
//! holds it.
//!
//! The tallies are split by what the page is for ([`Class`]), by the VMA the
//! page lies in; the page tables of a space are counted apart ([`Use::tables`]).
//!
//! Single-CPU: like the other walks of a foreign space's tables
//! (`filemap::write_protect`), this reads tables nobody can free while the
//! caller runs with interrupts off and no task switch.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use x86_64::PhysAddr;

use super::pte::{self, ADDR, DMA, HUGE, MMIO, PRESENT, USER};
use super::vma::{self, Kind};
use super::{frame_refcount, FRAME_SIZE, SHARED_WINDOW_INDEX, USER_TOP};

/// What a resident page is for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    /// The read-only segments of the program, shared with every process
    /// running it (`Kind::FileImage`): code and read-only data.
    Image,
    /// Memory the program owns: heap, anonymous mappings, written data.
    Private,
    /// Thread and process stacks.
    Stack,
    /// Memory shared on purpose: Messenger buffers, shared mappings of memfds,
    /// files and anonymous memory.
    Shared,
    /// Private file mappings (fonts, data files).
    Mapped,
}

/// The number of [`Class`] values.
pub const CLASSES: usize = 5;

impl Class {
    /// The class's position in a [`Use`] array.
    pub const fn index(self) -> usize {
        self as usize
    }

    fn of_kind(kind: Kind) -> Class {
        match kind {
            Kind::FileImage => Class::Image,
            Kind::Stack => Class::Stack,
            Kind::Shared | Kind::SharedAnon | Kind::FileShared => Class::Shared,
            Kind::FilePrivate => Class::Mapped,
            Kind::Anon | Kind::File | Kind::Heap => Class::Private,
        }
    }
}

/// What one address space holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Use {
    /// Resident user pages (every mapped leaf counts in full).
    pub rss: u64,
    /// Resident pages no other space maps (USS).
    pub private: u64,
    /// Proportional size in bytes: each page divided by its mappings (PSS).
    pub pss: u64,
    /// Page-table frames of the space, the top level included.
    pub tables: u64,
    /// Resident pages by [`Class`].
    pub class_rss: [u64; CLASSES],
    /// Proportional bytes by [`Class`]; they add up to [`Use::pss`].
    pub class_pss: [u64; CLASSES],
}

impl Use {
    fn add(&mut self, class: Class, mappers: u64) {
        let share = FRAME_SIZE / mappers.max(1);
        self.rss += 1;
        self.pss += share;
        if mappers <= 1 {
            self.private += 1;
        }
        self.class_rss[class.index()] += 1;
        self.class_pss[class.index()] += share;
    }
}

/// Shared leaves remembered at most; past it a leaf is divided by its frame's
/// reference count instead of its counted mappings (a bound on the scratch
/// memory of a machine that maps gigabytes of shared pages).
const MAX_SHARED_LEAVES: usize = 1 << 19;

/// A leaf whose share waits for the whole walk.
struct Shared {
    space: usize,
    class: Class,
    frame: u64,
}

/// The state of one [`measure`].
struct Tally {
    uses: Vec<Use>,
    waiting: Vec<Shared>,
    /// Mappings counted per frame, for the frames in `waiting`.
    mappings: BTreeMap<u64, u32>,
}

/// Measure each of `tables` (PML4s of live user address spaces).
pub fn measure(tables: &[PhysAddr]) -> Vec<Use> {
    let mut tally = Tally {
        uses: alloc::vec![Use::default(); tables.len()],
        waiting: Vec::new(),
        mappings: BTreeMap::new(),
    };
    for (space, table) in tables.iter().enumerate() {
        walk_space(&mut tally, space, *table);
    }
    tally.settle();
    tally.uses
}

impl Tally {
    /// Tally one present user leaf of `space`.
    fn leaf(&mut self, space: usize, class: Class, window: bool, entry: u64) {
        let frame = entry & ADDR;
        let refs = frame_refcount(PhysAddr::new(frame));
        if refs == 0 || refs == super::RESERVED {
            // Not an allocator frame (device memory, a driver's buffer).
            return;
        }
        if refs == 1 && !window {
            self.uses[space].add(class, 1);
            return;
        }
        if self.waiting.len() >= MAX_SHARED_LEAVES {
            self.uses[space].add(class, u64::from(refs));
            return;
        }
        *self.mappings.entry(frame).or_insert(0) += 1;
        self.waiting.push(Shared {
            space,
            class,
            frame,
        });
    }

    /// Divide the remembered leaves between their frames' mappings.
    fn settle(&mut self) {
        for (index, leaf) in self.waiting.iter().enumerate() {
            if index % 256 == 0 {
                crate::arch::irq_window::poll_point();
            }
            let mappers = self.mappings.get(&leaf.frame).copied().unwrap_or(1);
            self.uses[leaf.space].add(leaf.class, u64::from(mappers));
        }
    }
}

/// Walk the user half of `table`, tallying its leaves and table frames.
fn walk_space(tally: &mut Tally, space: usize, table: PhysAddr) {
    let vmas = vma::list(table);
    let mut cursor = Cursor { vmas, next: 0 };
    tally.uses[space].tables += 1;
    for index in 0..=SHARED_WINDOW_INDEX {
        // SAFETY: `table` is the PML4 of a live user address space, so it is
        // reachable through the physical map and `index` is below 512.
        let entry = unsafe { pte::read(table, index) };
        if entry & PRESENT != 0 {
            let base = (index as u64) << 39;
            let window = index == SHARED_WINDOW_INDEX;
            // SAFETY: a present PML4 entry names a PDPT.
            unsafe { walk_level(tally, space, &mut cursor, window, entry & ADDR, 3, base) };
        }
    }
}

/// The VMAs of a space and a cursor over them: leaves arrive in address
/// order, so the VMA of each is found by moving forward.
struct Cursor {
    vmas: Vec<vma::Vma>,
    next: usize,
}

impl Cursor {
    fn class_of(&mut self, va: u64) -> Class {
        if va >= USER_TOP {
            return Class::Shared;
        }
        while self.next < self.vmas.len() && self.vmas[self.next].end <= va {
            self.next += 1;
        }
        self.vmas
            .get(self.next)
            .filter(|vma| vma.start <= va)
            .map_or(Class::Private, |vma| Class::of_kind(vma.kind))
    }
}

/// Walk the table `phys` of `level` (3 = PDPT .. 1 = PT) whose first entry
/// covers `base`.
///
/// # Safety
/// `phys` must be a page table of `level` of a live address space.
unsafe fn walk_level(
    tally: &mut Tally,
    space: usize,
    cursor: &mut Cursor,
    window: bool,
    phys: u64,
    level: u8,
    base: u64,
) {
    crate::arch::irq_window::poll_point();
    tally.uses[space].tables += 1;
    // What one entry of this level maps: 1 GiB, 2 MiB, 4 KiB.
    let shift = 12 + 9 * (u32::from(level) - 1);
    for index in 0..512usize {
        let entry = pte::read(PhysAddr::new(phys), index);
        if entry & PRESENT == 0 {
            continue;
        }
        let va = base + ((index as u64) << shift);
        if level == 1 {
            if entry & USER != 0 && entry & (MMIO | DMA) == 0 {
                let class = cursor.class_of(va);
                tally.leaf(space, class, window, entry);
            }
        } else if entry & HUGE == 0 {
            walk_level(tally, space, cursor, window, entry & ADDR, level - 1, va);
        }
    }
}
