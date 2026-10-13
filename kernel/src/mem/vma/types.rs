//! The types a VMA is made of: protection, purpose, the range itself.

use alloc::sync::Arc;

use crate::fs::openfile::OpenFile;

/// Access protection bits. The numeric values match Linux `PROT_*`, so a
/// syscall argument can be masked straight into a `Prot`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Prot(pub u8);

impl Prot {
    pub const READ: Prot = Prot(1);
    pub const WRITE: Prot = Prot(2);
    pub const EXEC: Prot = Prot(4);

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

/// What a range backs. Anonymous memory, heaps and stacks are demand-zero;
/// file segments are loaded eagerly from the ELF image.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Anonymous private memory (`mmap(MAP_ANONYMOUS)`, scratch mappings).
    Anon,
    /// A `PT_LOAD` segment mapped eagerly from an ELF file.
    File,
    /// A user stack. The pages holding the start frame are mapped eagerly
    /// (the loader writes them before the task first runs); the rest of the
    /// reservation is populated on first touch.
    Stack,
    /// A `brk`/`sbrk` heap, populated on first touch.
    Heap,
    /// A `MAP_SHARED` mapping of a memfd, mapped eagerly. Its pages belong to
    /// the memfd, which holds the user-memory charge, so unmapping it gives
    /// no quota back; `fork` keeps the pages shared (`pte::SHARED`).
    Shared,
    /// A `MAP_SHARED | MAP_ANONYMOUS` mapping, mapped eagerly and shared with
    /// `fork` children. The mapping's creator holds the charge, as for
    /// [`Kind::Anon`].
    SharedAnon,
    /// A `MAP_PRIVATE` mapping of a regular file: demand-faulted from the
    /// file's page cache, copy-on-write, charged like anonymous memory. The
    /// VMA names its file ([`Vma::file`], [`Vma::file_off`]).
    FilePrivate,
    /// A `MAP_SHARED` mapping of a regular file: demand-faulted frames of
    /// the file's page cache that every mapper shares (`pte::SHARED`), written
    /// back to the file. Charged to the mapper like [`Kind::SharedAnon`].
    FileShared,
}

impl Kind {
    /// Whether the range is a file-backed mapping that names its file.
    pub fn is_file_mapping(self) -> bool {
        matches!(self, Kind::FilePrivate | Kind::FileShared)
    }
}

/// One page-aligned protection range: `start < end`, both 4 KiB aligned.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Vma {
    pub start: u64,
    pub end: u64,
    pub prot: Prot,
    pub kind: Kind,
    /// The file a file mapping maps, as an id into its address space's file
    /// table (`0` for every other kind). Ids are per address space and a
    /// `fork` child keeps its parent's.
    pub file: u32,
    /// The file offset of `start` (`0` without a file).
    pub file_off: u64,
    /// The range is a *reservation* (`mmap` of `PROT_NONE` or with
    /// `MAP_NORESERVE`): address space a runtime sets aside and commits in
    /// pieces. Its user-memory quota charge is the pages resident in it, taken
    /// at the first touch (`mem::demand_resolve`) and given back as they are
    /// unmapped, not its length at `mmap`; it stays so through `mprotect`.
    /// Only anonymous private mappings carry it.
    pub reserve: bool,
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

    /// The part of the range inside `[start, end)`, which it must overlap;
    /// a file mapping keeps the file offset of the page each piece starts at.
    pub fn clipped(self, start: u64, end: u64) -> Vma {
        let first = self.start.max(start);
        Vma {
            start: first,
            end: self.end.min(end),
            file_off: self.offset_at(first),
            ..self
        }
    }

    /// The file offset of address `va` of the range (`0` without a file).
    pub fn offset_at(self, va: u64) -> u64 {
        if self.file == 0 {
            0
        } else {
            self.file_off + (va - self.start)
        }
    }
}

/// A removed piece of a range and the file it mapped, for the caller to
/// write back and let go of once the pages are unmapped.
pub struct Unmapped {
    pub vma: Vma,
    pub file: Option<Arc<OpenFile>>,
}
