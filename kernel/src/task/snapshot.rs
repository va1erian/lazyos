//! Shared, copy-on-write file snapshots for `Fd::File` descriptors.
//!
//! `open` copies a file into the kernel heap once; `dup`/`fork` then share
//! that copy through the `Arc` and only the first writer detaches its own.
//! Every allocation here is fallible: the heap is small (16 MiB), so running
//! out must fail the one call, never abort the kernel.

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::{current, Fd, FD_COUNT, TASKS};

/// Get `snapshot` ready for a write that will extend it to `end` bytes:
/// detach it from other holders and reserve the capacity. Done *before* the
/// backing file is written, so the later [`write_at`] cannot fail and a
/// committed write is never reported as failed.
pub fn prepare(snapshot: &mut Arc<Vec<u8>>, end: usize) -> bool {
    if Arc::get_mut(snapshot).is_none() {
        let mut own = Vec::new();
        if own.try_reserve_exact(snapshot.len().max(end)).is_err() {
            return false;
        }
        own.extend_from_slice(snapshot);
        *snapshot = Arc::new(own);
    }
    // INVARIANT: the branch above left this `Arc` uniquely owned.
    let bytes = Arc::get_mut(snapshot).expect("snapshot is unshared");
    let extra = end.saturating_sub(bytes.len());
    extra == 0 || bytes.try_reserve_exact(extra).is_ok()
}

/// Mirror a write of `data` at `offset` into `snapshot`, detaching it from
/// other holders first. Returns `false` (leaving the snapshot unchanged) on
/// arithmetic overflow or allocation failure.
///
/// The backing file already accepted this write and its size cap, so this
/// only keeps the descriptor's view current.
pub fn write_at(snapshot: &mut Arc<Vec<u8>>, offset: usize, data: &[u8]) -> bool {
    let Some(end) = offset.checked_add(data.len()) else {
        return false;
    };
    if Arc::get_mut(snapshot).is_none() {
        let mut own = Vec::new();
        if own.try_reserve_exact(snapshot.len().max(end)).is_err() {
            return false;
        }
        own.extend_from_slice(snapshot);
        *snapshot = Arc::new(own);
    }
    // INVARIANT: the branch above left this `Arc` uniquely owned.
    let bytes = Arc::get_mut(snapshot).expect("snapshot is unshared");
    if end > bytes.len() {
        if bytes.try_reserve_exact(end - bytes.len()).is_err() {
            return false;
        }
        bytes.resize(end, 0);
    }
    bytes[offset..end].copy_from_slice(data);
    true
}

/// [`prepare`] descriptor `fd` of the current task for a `len`-byte write at
/// `offset`. Returns `false` if `fd` is not a regular file, the range
/// overflows, or memory ran out; call it before committing the write.
pub fn prepare_fd_write(fd: usize, offset: usize, len: usize) -> bool {
    let Some(end) = offset.checked_add(len) else {
        return false;
    };
    let mut tasks = TASKS.lock();
    let Some(task) = tasks[current()].as_mut() else {
        return false;
    };
    match task.fds.get_mut(fd) {
        Some(Fd::File { data, .. }) if fd < FD_COUNT => prepare(data, end),
        _ => false,
    }
}

/// Set descriptor `fd`'s snapshot to exactly `len` bytes (zero-filling growth),
/// mirroring an `ftruncate` the backing file has already accepted. The caller
/// first ran [`prepare_fd_write`] for `len`, so the snapshot is unshared and the
/// capacity is reserved: this cannot fail.
pub fn fd_set_len(fd: usize, len: usize) {
    let mut tasks = TASKS.lock();
    let Some(task) = tasks[current()].as_mut() else {
        return;
    };
    if let Some(Fd::File { data, .. }) = task.fds.get_mut(fd) {
        if let Some(bytes) = Arc::get_mut(data) {
            bytes.resize(len, 0);
        }
    }
}
