//! The order dirty sectors reach the device in.
//!
//! FAT has no journal, so the order of the writes of one operation is the
//! only thing that decides what a cut (a pulled stick, a crash) leaves. The
//! three things an operation changes depend on each other one way:
//!
//! ```text
//!   file bytes  ◀── a chain in the FAT ◀── a directory entry (first
//!                                          cluster, size)
//! ```
//!
//! A reference must never be written before what it refers to. Growing a
//! file therefore writes the bytes first, then the FAT, then the entry
//! ([`Order::Grow`]): a cut leaves the file as it was, or its new clusters
//! allocated and unreferenced (which `fsck` frees), never an entry that
//! names bytes that were not written. Shrinking is the other way round
//! ([`Order::Shrink`]): the entry stops naming the clusters before the FAT
//! frees them, so a freed cluster is never still somebody's. Everything
//! else is written lowest sector first, which puts the FAT before the
//! directories that use it. The reserved sectors (the boot sector's dirty
//! mark) go first in every order.
//!
//! Two limits, both documented in `docs/architecture/usb-storage.md`: the
//! cache's dirty limit flushes mid-operation, so an operation that frees
//! more clusters than a megabyte of FAT sectors holds sends the FAT before
//! the entry; and a rename's two entries go by sector number, so a cut can
//! lose the name (never the data).

/// How the dirty sectors of one flush are ordered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Order {
    /// Lowest sector first.
    #[default]
    Ascending,
    /// New content (a file's bytes, a new directory's cluster), then
    /// everything else lowest first (the FAT, then the directories that
    /// name it).
    Grow,
    /// Directories (the data area, and a FAT12/16 root directory) lowest
    /// first, then the FAT and the structures before it.
    Shrink,
}

/// Where the structures end, in sectors from the start of the volume.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// The first FAT's first sector; the reserved sectors before it (the
    /// boot sector with its dirty bit, FSInfo) are written before anything
    /// the operation changes.
    pub fat_start: u64,
    /// First sector after the reserved sectors and the FATs: a FAT12/16
    /// root directory starts here, and so does everything that is not the
    /// FAT.
    pub fat_end: u64,
    /// First sector of the data area (cluster 2).
    pub data_start: u64,
}

/// Put `dirty` (ascending sector numbers) in the order `order` asks for;
/// `file_data` says whether a sector holds a file's bytes.
pub(super) fn arrange(
    dirty: &mut [u64],
    order: Order,
    layout: Option<Layout>,
    file_data: impl Fn(u64) -> bool,
) {
    // The reserved sectors first: the boot sector's dirty mark precedes every
    // change it stands for (without a layout there is none). The FATs are
    // not among them: their first sector holds entries that free clusters.
    let marker = |sector: u64| layout.is_some_and(|l| sector < l.fat_start);
    match (order, layout) {
        (Order::Grow, _) => dirty.sort_by_key(|&sector| {
            let rank = if marker(sector) {
                0
            } else if file_data(sector) {
                1
            } else {
                2
            };
            (rank, sector)
        }),
        (Order::Shrink, Some(layout)) => dirty.sort_by_key(|&sector| {
            let rank = if marker(sector) {
                0
            } else if sector >= layout.fat_end {
                1
            } else {
                2
            };
            (rank, sector)
        }),
        _ => {}
    }
}
