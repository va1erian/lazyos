//! Per-address-space virtual memory areas (VMAs).
//!
//! A [`Vma`] is a page-aligned `[start, end)` range plus its access protection
//! and its purpose. Page tables remain the source of truth for what is
//! *present*; the VMA list is the source of truth for what *may become*
//! present ([`crate::mem::demand_fault`] consults it) and for what `munmap`/
//! `mprotect` are allowed to touch.
//!
//! Lists are keyed by PML4 physical address in a global registry, the same
//! pattern as `task::BUMPS`: that keeps the `Task` struct unchanged (a
//! constraint of issue #55) while still sharing one list between all tasks of
//! an address space, as `clone(CLONE_VM)` threads require. When `Task` grows an
//! `AddressSpace` field, this registry can collapse into a plain field.

use alloc::vec::Vec;
use spin::Mutex;
use x86_64::PhysAddr;

/// Page size the VMA granularity is tied to (the kernel only maps 4 KiB pages).
const PAGE: u64 = 4096;

/// Access protection bits. The numeric values match Linux `PROT_*`, so a
/// syscall argument can be masked straight into a `Prot`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Prot(pub u8);

impl Prot {
    pub const READ: Prot = Prot(1);
    pub const WRITE: Prot = Prot(2);
    pub const EXEC: Prot = Prot(4);

    pub fn has_read(self) -> bool {
        self.0 & Self::READ.0 != 0
    }

    pub fn has_write(self) -> bool {
        self.0 & Self::WRITE.0 != 0
    }

    pub fn has_exec(self) -> bool {
        self.0 & Self::EXEC.0 != 0
    }
}

impl core::ops::BitOr for Prot {
    type Output = Prot;

    fn bitor(self, other: Prot) -> Prot {
        Prot(self.0 | other.0)
    }
}

/// What a range backs. Only anonymous memory is demand-zero today; file
/// segments are loaded eagerly from the ELF image.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Anonymous private memory (`mmap(MAP_ANONYMOUS)`, scratch mappings).
    Anon,
    /// A `PT_LOAD` segment mapped eagerly from an ELF file.
    File,
    /// A user stack. Mapped eagerly so the loader can write the start frame
    /// before the task first runs.
    Stack,
    /// A `brk`/`sbrk` heap, populated on first touch.
    Heap,
}

/// One page-aligned protection range: `start < end`, both 4 KiB aligned.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Vma {
    pub start: u64,
    pub end: u64,
    pub prot: Prot,
    pub kind: Kind,
}

impl Vma {
    /// Size of the range in bytes (accounting/diagnostics).
    #[allow(dead_code)] // exercised by `vma_stats` from tests and tools
    pub fn len(self) -> u64 {
        self.end - self.start
    }

    /// Whether `va` lies inside the range.
    pub fn contains(self, va: u64) -> bool {
        va >= self.start && va < self.end
    }

    /// Whether the range overlaps `[start, end)`.
    pub fn intersects(self, start: u64, end: u64) -> bool {
        start < self.end && end > self.start
    }
}

/// The VMAs of one address space, keyed by PML4 frame.
struct Space {
    pml4: u64,
    vmas: Vec<Vma>,
}

/// All address spaces' VMA lists. A `Vec` scan is fine at this scale (a
/// handful of spaces, a handful of VMAs each).
static SPACES: Mutex<Vec<Space>> = Mutex::new(Vec::new());

fn align_down(address: u64) -> u64 {
    address & !(PAGE - 1)
}

fn align_up(address: u64) -> u64 {
    // Saturating: an end address near `u64::MAX` just stays on its page.
    address.saturating_add(PAGE - 1) & !(PAGE - 1)
}

fn find_space(spaces: &mut Vec<Space>, table: PhysAddr) -> Option<&mut Space> {
    spaces.iter_mut().find(|space| space.pml4 == table.as_u64())
}

fn ensure_space<'a>(spaces: &'a mut Vec<Space>, table: PhysAddr) -> &'a mut Space {
    let pml4 = table.as_u64();
    if spaces.iter().all(|space| space.pml4 != pml4) {
        spaces.push(Space {
            pml4,
            vmas: Vec::new(),
        });
    }
    find_space(spaces, table).expect("space was just inserted")
}

/// Start tracking a fresh address space. Reusing a recycled PML4 frame must not
/// inherit the previous address space's VMAs, so an existing entry is reset.
pub fn register(table: PhysAddr) {
    let mut spaces = SPACES.lock();
    match find_space(&mut spaces, table) {
        Some(space) => space.vmas.clear(),
        None => spaces.push(Space {
            pml4: table.as_u64(),
            vmas: Vec::new(),
        }),
    }
}

/// Drop an address space's list (called when its PML4 is torn down).
pub fn forget(table: PhysAddr) {
    SPACES.lock().retain(|space| space.pml4 != table.as_u64());
}

/// Give `child` a copy of `parent`'s list (fork keeps both sides' layout).
pub fn clone_space(parent: PhysAddr, child: PhysAddr) {
    let mut spaces = SPACES.lock();
    let vmas = find_space(&mut spaces, parent)
        .map(|space| space.vmas.clone())
        .unwrap_or_default();
    match find_space(&mut spaces, child) {
        Some(space) => space.vmas = vmas,
        None => spaces.push(Space {
            pml4: child.as_u64(),
            vmas,
        }),
    }
}

/// Record `[start, end)` with `prot`/`kind`, coalescing with adjacent VMAs that
/// agree and overwriting any different mapping already inside the range
/// (`MAP_FIXED` semantics: the new intent wins).
pub fn insert(table: PhysAddr, start: u64, end: u64, prot: Prot, kind: Kind) {
    let start = align_down(start);
    let end = align_up(end);
    if end <= start {
        return;
    }
    let mut spaces = SPACES.lock();
    insert_in(
        &mut ensure_space(&mut spaces, table).vmas,
        start,
        end,
        prot,
        kind,
    );
}

fn insert_in(vmas: &mut Vec<Vma>, start: u64, end: u64, prot: Prot, kind: Kind) {
    let mut kept: Vec<Vma> = Vec::with_capacity(vmas.len() + 2);
    for vma in vmas.drain(..) {
        if !vma.intersects(start, end) {
            kept.push(vma);
            continue;
        }
        // Keep the parts outside the new range; the covered parts are replaced.
        if vma.start < start {
            kept.push(Vma { end: start, ..vma });
        }
        if vma.end > end {
            kept.push(Vma { start: end, ..vma });
        }
    }
    kept.push(Vma {
        start,
        end,
        prot,
        kind,
    });
    kept.sort_unstable_by_key(|vma| vma.start);
    merge(kept, vmas);
}

/// Remove `[start, end)`, splitting partially covered VMAs. Returns the removed
/// pieces (clipped to the range) so callers can report what was unmapped.
pub fn remove(table: PhysAddr, start: u64, end: u64) -> Vec<Vma> {
    let start = align_down(start);
    let end = align_up(end);
    let mut removed = Vec::new();
    if end <= start {
        return removed;
    }
    let mut spaces = SPACES.lock();
    if let Some(space) = find_space(&mut spaces, table) {
        let mut kept: Vec<Vma> = Vec::with_capacity(space.vmas.len() + 2);
        for vma in space.vmas.drain(..) {
            if !vma.intersects(start, end) {
                kept.push(vma);
                continue;
            }
            if vma.start < start {
                kept.push(Vma { end: start, ..vma });
            }
            if vma.end > end {
                kept.push(Vma { start: end, ..vma });
            }
            removed.push(Vma {
                start: vma.start.max(start),
                end: vma.end.min(end),
                ..vma
            });
        }
        space.vmas = kept;
    }
    removed
}

/// The VMA containing `va`, if any.
pub fn find(table: PhysAddr, va: u64) -> Option<Vma> {
    let spaces = SPACES.lock();
    let space = spaces.iter().find(|space| space.pml4 == table.as_u64())?;
    space.vmas.iter().copied().find(|vma| vma.contains(va))
}

/// Every VMA intersecting `[start, end)`, clipped to the range.
pub fn find_range(table: PhysAddr, start: u64, end: u64) -> Vec<Vma> {
    let mut found = Vec::new();
    if end <= start {
        return found;
    }
    let spaces = SPACES.lock();
    if let Some(space) = spaces.iter().find(|space| space.pml4 == table.as_u64()) {
        for vma in &space.vmas {
            if vma.intersects(start, end) {
                found.push(Vma {
                    start: vma.start.max(start),
                    end: vma.end.min(end),
                    ..*vma
                });
            }
        }
    }
    found
}

/// Set `prot` on every VMA intersecting `[start, end)`, splitting at the range
/// boundaries and re-merging what the new protection makes adjacent. Returns
/// whether the range covered at least one existing VMA.
pub fn protect(table: PhysAddr, start: u64, end: u64, prot: Prot) -> bool {
    let start = align_down(start);
    let end = align_up(end);
    if end <= start {
        return false;
    }
    let mut spaces = SPACES.lock();
    let Some(space) = find_space(&mut spaces, table) else {
        return false;
    };
    let vmas = &mut space.vmas;
    let covered = vmas.iter().any(|vma| vma.intersects(start, end));
    // Split boundary VMAs so exactly the covered parts get the new protection.
    split_at(vmas, start);
    split_at(vmas, end);
    for vma in vmas.iter_mut() {
        if vma.start >= start && vma.end <= end {
            vma.prot = prot;
        }
    }
    let merged = core::mem::take(vmas);
    merge(merged, vmas);
    covered
}

/// Split the VMA containing `address` (strictly inside it) into two.
fn split_at(vmas: &mut Vec<Vma>, address: u64) {
    let Some(index) = vmas
        .iter()
        .position(|vma| vma.start < address && address < vma.end)
    else {
        return;
    };
    let original = vmas[index];
    vmas[index].end = address;
    vmas.insert(
        index + 1,
        Vma {
            start: address,
            ..original
        },
    );
}

/// A snapshot of an address space's VMAs, for diagnostics and tests (the
/// iteration surface; a future `/proc/self/maps` would use this too).
#[allow(dead_code)]
pub fn list(table: PhysAddr) -> Vec<Vma> {
    let spaces = SPACES.lock();
    spaces
        .iter()
        .find(|space| space.pml4 == table.as_u64())
        .map(|space| space.vmas.clone())
        .unwrap_or_default()
}

/// Coalesce adjacent VMAs that share protection and purpose. `vmas` must be
/// sorted by `start`; the result is written back into `out`.
fn merge(vmas: Vec<Vma>, out: &mut Vec<Vma>) {
    out.clear();
    for vma in vmas {
        match out.last_mut() {
            Some(last)
                if last.end == vma.start && last.prot == vma.prot && last.kind == vma.kind =>
            {
                last.end = vma.end;
            }
            _ => out.push(vma),
        }
    }
}
