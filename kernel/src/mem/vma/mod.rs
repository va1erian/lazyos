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

use alloc::sync::Arc;
use alloc::vec::Vec;
use spin::Mutex;
use x86_64::PhysAddr;

use crate::fs::openfile::OpenFile;

mod types;
pub use types::{Kind, Prot, Unmapped, Vma};

/// Page size the VMA granularity is tied to (the kernel only maps 4 KiB pages).
const PAGE: u64 = 4096;

/// The VMAs of one address space, keyed by PML4 frame.
struct Space {
    pml4: u64,
    vmas: Vec<Vma>,
    /// The files file mappings of this space name, by [`Vma::file`] id.
    files: Vec<(u32, Arc<OpenFile>)>,
    /// The id the next file mapping gets.
    next_file: u32,
}

impl Space {
    fn new(pml4: u64) -> Space {
        Space {
            pml4,
            vmas: Vec::new(),
            files: Vec::new(),
            next_file: 1,
        }
    }

    fn file(&self, id: u32) -> Option<Arc<OpenFile>> {
        self.files
            .iter()
            .find(|(file, _)| *file == id)
            .map(|(_, file)| Arc::clone(file))
    }

    /// Forget the files no VMA names any more; the caller drops them after
    /// the registry lock is released (the last close of a file can do I/O).
    fn prune(&mut self) -> Vec<Arc<OpenFile>> {
        let vmas = &self.vmas;
        let mut dropped = Vec::new();
        let mut index = 0;
        while index < self.files.len() {
            let id = self.files[index].0;
            if vmas.iter().any(|vma| vma.file == id) {
                index += 1;
            } else {
                dropped.push(self.files.remove(index).1);
            }
        }
        dropped
    }
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

fn find_space(spaces: &mut [Space], table: PhysAddr) -> Option<&mut Space> {
    spaces.iter_mut().find(|space| space.pml4 == table.as_u64())
}

#[allow(clippy::expect_used)] // INVARIANT: see the comment at the lookup
fn ensure_space(spaces: &mut Vec<Space>, table: PhysAddr) -> &mut Space {
    let pml4 = table.as_u64();
    if spaces.iter().all(|space| space.pml4 != pml4) {
        spaces.push(Space::new(pml4));
    }
    // INVARIANT: the branch above just pushed a `Space` with this exact
    // `pml4`, and nothing between the push and this lookup can remove it
    // (single-threaded, no reentrancy here), so the find always succeeds.
    find_space(spaces, table).expect("space was just inserted")
}

/// Start tracking a fresh address space. Reusing a recycled PML4 frame must not
/// inherit the previous address space's VMAs, so an existing entry is reset.
pub fn register(table: PhysAddr) {
    let old = {
        let mut spaces = SPACES.lock();
        match find_space(&mut spaces, table) {
            Some(space) => Some(core::mem::replace(space, Space::new(table.as_u64()))),
            None => {
                spaces.push(Space::new(table.as_u64()));
                None
            }
        }
    };
    drop(old);
}

/// Drop an address space's list (called when its PML4 is torn down). The
/// files its mappings held go with it, after the registry lock is released.
pub fn forget(table: PhysAddr) {
    let gone: Vec<Space> = {
        let mut spaces = SPACES.lock();
        let (gone, kept): (Vec<Space>, Vec<Space>) = core::mem::take(&mut *spaces)
            .into_iter()
            .partition(|space| space.pml4 == table.as_u64());
        *spaces = kept;
        gone
    };
    drop(gone);
}

/// Give `child` a copy of `parent`'s list (fork keeps both sides' layout, and
/// the child's file mappings name the same open files).
pub fn clone_space(parent: PhysAddr, child: PhysAddr) {
    let mut spaces = SPACES.lock();
    let (vmas, files, next_file) = find_space(&mut spaces, parent)
        .map(|space| (space.vmas.clone(), space.files.clone(), space.next_file))
        .unwrap_or_else(|| (Vec::new(), Vec::new(), 1));
    let replaced = match find_space(&mut spaces, child) {
        Some(space) => Some(core::mem::replace(
            space,
            Space {
                pml4: child.as_u64(),
                vmas,
                files,
                next_file,
            },
        )),
        None => {
            spaces.push(Space {
                pml4: child.as_u64(),
                vmas,
                files,
                next_file,
            });
            None
        }
    };
    drop(spaces);
    drop(replaced);
}

/// Record `[start, end)` with `prot`/`kind`, coalescing with adjacent VMAs that
/// agree and overwriting any different mapping already inside the range
/// (`MAP_FIXED` semantics: the new intent wins).
pub fn insert(table: PhysAddr, start: u64, end: u64, prot: Prot, kind: Kind) {
    insert_mapped(table, start, end, prot, kind, None);
}

/// [`insert`] for a file mapping: `file` is the open file, mapped from
/// offset `offset` at `start`.
pub fn insert_file(
    table: PhysAddr,
    start: u64,
    end: u64,
    prot: Prot,
    kind: Kind,
    file: Arc<OpenFile>,
    offset: u64,
) {
    insert_mapped(table, start, end, prot, kind, Some((file, offset)));
}

fn insert_mapped(
    table: PhysAddr,
    start: u64,
    end: u64,
    prot: Prot,
    kind: Kind,
    file: Option<(Arc<OpenFile>, u64)>,
) {
    let start = align_down(start);
    let end = align_up(end);
    if end <= start {
        return;
    }
    let dropped = {
        let mut spaces = SPACES.lock();
        let space = ensure_space(&mut spaces, table);
        let (id, file_off) = match file {
            Some((file, offset)) => {
                let id = space.next_file;
                space.next_file = id.wrapping_add(1).max(1);
                space.files.push((id, file));
                (id, offset)
            }
            None => (0, 0),
        };
        insert_in(
            &mut space.vmas,
            Vma {
                start,
                end,
                prot,
                kind,
                file: id,
                file_off,
            },
        );
        space.prune()
    };
    drop(dropped);
}

fn insert_in(vmas: &mut Vec<Vma>, new: Vma) {
    let mut kept: Vec<Vma> = Vec::with_capacity(vmas.len() + 2);
    for vma in vmas.drain(..) {
        if !vma.intersects(new.start, new.end) {
            kept.push(vma);
            continue;
        }
        // Keep the parts outside the new range; the covered parts are replaced.
        if vma.start < new.start {
            kept.push(vma.clipped(vma.start, new.start));
        }
        if vma.end > new.end {
            kept.push(vma.clipped(new.end, vma.end));
        }
    }
    kept.push(new);
    kept.sort_unstable_by_key(|vma| vma.start);
    merge(kept, vmas);
}

/// Remove `[start, end)`, splitting partially covered VMAs. Returns the removed
/// pieces (clipped to the range) so callers can report what was unmapped.
pub fn remove(table: PhysAddr, start: u64, end: u64) -> Vec<Vma> {
    remove_files(table, start, end)
        .into_iter()
        .map(|piece| piece.vma)
        .collect()
}

/// [`remove`], each piece with the file it mapped. The caller unmaps the
/// pages and then calls `mem::filemap::released` so the file's pages are
/// written back and let go.
pub fn remove_files(table: PhysAddr, start: u64, end: u64) -> Vec<Unmapped> {
    let start = align_down(start);
    let end = align_up(end);
    let mut removed = Vec::new();
    if end <= start {
        return removed;
    }
    let dropped = {
        let mut spaces = SPACES.lock();
        let Some(space) = find_space(&mut spaces, table) else {
            return removed;
        };
        let mut kept: Vec<Vma> = Vec::with_capacity(space.vmas.len() + 2);
        let old = core::mem::take(&mut space.vmas);
        for vma in old {
            if !vma.intersects(start, end) {
                kept.push(vma);
                continue;
            }
            if vma.start < start {
                kept.push(vma.clipped(vma.start, start));
            }
            if vma.end > end {
                kept.push(vma.clipped(end, vma.end));
            }
            removed.push(Unmapped {
                vma: vma.clipped(start, end),
                file: space.file(vma.file),
            });
        }
        space.vmas = kept;
        space.prune()
    };
    drop(dropped);
    removed
}

/// The VMA containing `va`, if any.
pub fn find(table: PhysAddr, va: u64) -> Option<Vma> {
    let spaces = SPACES.lock();
    let space = spaces.iter().find(|space| space.pml4 == table.as_u64())?;
    space.vmas.iter().copied().find(|vma| vma.contains(va))
}

/// The open file the file mapping `vma` of `table` maps.
pub fn file_of(table: PhysAddr, vma: &Vma) -> Option<Arc<OpenFile>> {
    if vma.file == 0 {
        return None;
    }
    let spaces = SPACES.lock();
    spaces
        .iter()
        .find(|space| space.pml4 == table.as_u64())?
        .file(vma.file)
}

/// Every file mapping of `table` with its file (clones of the references).
pub fn file_mappings(table: PhysAddr) -> Vec<Unmapped> {
    let spaces = SPACES.lock();
    let Some(space) = spaces.iter().find(|space| space.pml4 == table.as_u64()) else {
        return Vec::new();
    };
    space
        .vmas
        .iter()
        .filter(|vma| vma.file != 0)
        .map(|vma| Unmapped {
            vma: *vma,
            file: space.file(vma.file),
        })
        .collect()
}

/// Every `MAP_SHARED` file mapping of every address space: its table, its
/// range and its file (write-back write-protects them).
pub fn shared_file_mappings() -> Vec<(PhysAddr, Vma, Arc<OpenFile>)> {
    let spaces = SPACES.lock();
    let mut out = Vec::new();
    for space in spaces.iter() {
        for vma in space.vmas.iter().filter(|vma| vma.kind == Kind::FileShared) {
            if let Some(file) = space.file(vma.file) {
                out.push((PhysAddr::new(space.pml4), *vma, file));
            }
        }
    }
    out
}

/// Take `table`'s file mappings out of its VMA list (and forget the list):
/// the teardown order is "forget, release the PML4 frame, then write back",
/// so a recycled PML4 frame never meets a stale list.
pub fn take_file_mappings(table: PhysAddr) -> Vec<Unmapped> {
    let taken = file_mappings(table);
    forget(table);
    taken
}

/// Whether `va` lies in a VMA that does not permit writing. A page with no
/// VMA (unit tests map frames by hand) is not second-guessed. Copy-on-write
/// asks it: a private view of a memfd is mapped COW whatever its protection,
/// so that `mprotect(PROT_WRITE)` cannot reach the file's frame, and a store
/// to a read-only view must stay a fault.
pub fn forbids_write(table: PhysAddr, va: u64) -> bool {
    find(table, va).is_some_and(|vma| !vma.prot.has_write())
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
                found.push(vma.clipped(start, end));
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

/// Split the VMA containing `address` (strictly inside it) in two, so its two
/// sides can be treated as separate ranges. Returns whether a split happened.
pub fn split(table: PhysAddr, address: u64) -> bool {
    let address = align_down(address);
    let mut spaces = SPACES.lock();
    let Some(space) = find_space(&mut spaces, table) else {
        return false;
    };
    let before = space.vmas.len();
    split_at(&mut space.vmas, address);
    space.vmas.len() != before
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
    vmas.insert(index + 1, original.clipped(address, original.end));
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
                if last.end == vma.start
                    && last.prot == vma.prot
                    && last.kind == vma.kind
                    && last.file == vma.file
                    // The same file only merges where the offsets continue.
                    && (last.file == 0 || last.offset_at(last.end) == vma.file_off) =>
            {
                last.end = vma.end;
            }
            _ => out.push(vma),
        }
    }
}
