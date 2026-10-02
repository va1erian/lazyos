//! Snapshot open file descriptions: what an `Fd::File` descriptor refers to.
//!
//! `open` copies a file into the kernel heap once. That copy, the read/write
//! position and what the open recorded about the file (its path, access mode,
//! `O_APPEND`) form one open file description, a [`SnapFile`]. `dup`, `dup2`,
//! `fcntl(F_DUPFD)`, `fork` and `execve` share it through the `Arc`, as POSIX
//! requires: a shell's `prog >out 2>&1` writes stdout and stderr at one
//! advancing offset, and a child writes through a descriptor its parent opened
//! because the description carries the path its writes go to.
//!
//! Every allocation here is fallible: the heap is small (16 MiB), so running
//! out must fail the one call, never abort the kernel.
//!
//! Lock order: the task table, then (after the table lock is released) one
//! description's state; nothing here takes the table while holding a state.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use spin::Mutex;

use super::{current, Fd, FD_COUNT, TASKS};

/// Most bytes one [`fd_peek`] hands back; a short read is legal, so a huge
/// request is served in pieces instead of duplicating the whole file.
const FD_READ_MAX: usize = 1 << 20;

/// What an open recorded about the file behind a snapshot description, for
/// `fstat` and for `write`, which updates the backing file by `path`.
#[derive(Clone)]
pub struct FileMeta {
    /// `st_mode` at open time (type and permission bits).
    pub mode: u32,
    pub ino: u64,
    /// Owner at open time, so `fstat`/`statx` report it like `stat` does.
    pub uid: u32,
    pub gid: u32,
    /// Absolute ABI path backing a real file or directory open; `None` for
    /// the synthetic device descriptors.
    pub path: Option<String>,
    /// Whether the description accepts `write(2)` (the open had an access
    /// mode other than `O_RDONLY`).
    pub writable: bool,
    /// `O_APPEND`: writes ignore the position and land at EOF.
    pub append: bool,
    /// Synthetic nodes (`/dev/null`, `/dev/zero`, `/dev/full`): writes are
    /// discarded and reads return the snapshot (empty).
    pub device: bool,
}

/// One open file description over a heap snapshot of a file.
pub struct SnapFile {
    state: Mutex<SnapState>,
    meta: Option<FileMeta>,
}

/// The mutable half of a [`SnapFile`].
struct SnapState {
    data: Vec<u8>,
    offset: usize,
}

impl SnapFile {
    /// What the open recorded; `None` for a description made without it.
    pub fn meta(&self) -> Option<&FileMeta> {
        self.meta.as_ref()
    }
}

impl Fd {
    /// A new snapshot description over `data`, positioned at its start.
    pub fn file(data: Vec<u8>, meta: Option<FileMeta>) -> Fd {
        Fd::File {
            file: Arc::new(SnapFile {
                state: Mutex::new(SnapState { data, offset: 0 }),
                meta,
            }),
        }
    }
}

/// The snapshot description behind descriptor `fd` of the current task.
fn snap(fd: usize) -> Option<Arc<SnapFile>> {
    let tasks = TASKS.lock();
    let task = tasks[current()].as_ref()?;
    match task.fds.get(fd) {
        Some(Fd::File { file }) if fd < FD_COUNT => Some(Arc::clone(file)),
        _ => None,
    }
}

/// What the open of descriptor `fd` recorded, if it is a snapshot file that
/// recorded anything.
pub fn fd_file_meta(fd: usize) -> Option<FileMeta> {
    snap(fd)?.meta().cloned()
}

/// Up to `count` bytes of `data` from `offset` (empty at or past EOF).
fn chunk(data: &[u8], offset: usize, count: usize) -> Vec<u8> {
    if offset >= data.len() {
        return Vec::new();
    }
    let n = (data.len() - offset).min(count).min(FD_READ_MAX);
    data[offset..offset + n].to_vec()
}

/// The next up-to-`count` bytes of a file descriptor, *without* advancing its
/// offset. The bytes come back in a kernel buffer so the caller can copy them
/// to user memory through the validated path and only then [`fd_advance`].
pub fn fd_peek(fd: usize, count: usize) -> Option<Vec<u8>> {
    let file = snap(fd)?;
    let state = file.state.lock();
    Some(chunk(&state.data, state.offset, count))
}

/// Like [`fd_peek`] but from an explicit `offset` (`pread(2)`), which the
/// descriptor's own position takes no part in.
pub fn fd_peek_at(fd: usize, offset: usize, count: usize) -> Option<Vec<u8>> {
    let file = snap(fd)?;
    let state = file.state.lock();
    Some(chunk(&state.data, offset, count))
}

/// Advance a file descriptor's offset by `n` bytes after a successful
/// [`fd_peek`] and copy-out.
pub fn fd_advance(fd: usize, n: usize) {
    if let Some(file) = snap(fd) {
        let mut state = file.state.lock();
        state.offset = state.offset.saturating_add(n);
    }
}

/// Read up to `count` bytes from a file descriptor and advance its offset.
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // the tests read through it
pub fn fd_read(fd: usize, count: usize) -> Option<Vec<u8>> {
    let file = snap(fd)?;
    let mut state = file.state.lock();
    let bytes = chunk(&state.data, state.offset, count);
    state.offset += bytes.len();
    Some(bytes)
}

/// File size for a file descriptor (none for terminals/closed).
pub fn fd_size(fd: usize) -> Option<u64> {
    Some(snap(fd)?.state.lock().data.len() as u64)
}

/// The current read/write position of a file descriptor.
pub fn fd_offset(fd: usize) -> Option<usize> {
    Some(snap(fd)?.state.lock().offset)
}

/// Reserve room for `snapshot` to grow to `end` bytes.
fn reserve(snapshot: &mut Vec<u8>, end: usize) -> bool {
    let extra = end.saturating_sub(snapshot.len());
    extra == 0 || snapshot.try_reserve_exact(extra).is_ok()
}

/// Copy `data` into `snapshot` at `offset`, extending (and zero-filling) as
/// needed. Returns `false` (leaving the snapshot unchanged) on arithmetic
/// overflow or allocation failure.
fn write_at(snapshot: &mut Vec<u8>, offset: usize, data: &[u8]) -> bool {
    let Some(end) = offset.checked_add(data.len()) else {
        return false;
    };
    if !reserve(snapshot, end) {
        return false;
    }
    if end > snapshot.len() {
        snapshot.resize(end, 0);
    }
    snapshot[offset..end].copy_from_slice(data);
    true
}

/// Get descriptor `fd`'s snapshot ready for a `len`-byte write at `offset` by
/// reserving the capacity. Done *before* the backing file is written, so the
/// mirroring [`fd_apply_write`] does not run out of memory and a committed
/// write is not reported as failed. Returns `false` if `fd` is not a regular
/// file, the range overflows, or memory ran out.
pub fn prepare_fd_write(fd: usize, offset: usize, len: usize) -> bool {
    let Some(end) = offset.checked_add(len) else {
        return false;
    };
    match snap(fd) {
        Some(file) => reserve(&mut file.state.lock().data, end),
        None => false,
    }
}

/// Patch `data` into a file descriptor's snapshot at `offset` and move the
/// shared position past it. Returns false unless the descriptor holds a
/// regular file; the Linux ABI uses this to make a writable fd read back its
/// own writes after the backing file was updated.
pub fn fd_apply_write(fd: usize, offset: usize, data: &[u8]) -> bool {
    let Some(file) = snap(fd) else {
        return false;
    };
    let mut state = file.state.lock();
    if !write_at(&mut state.data, offset, data) {
        return false;
    }
    state.offset = offset + data.len(); // cannot overflow: `write_at` checked it
    true
}

/// [`fd_apply_write`] for a positional write (`pwrite64`), which leaves the
/// position where it was.
pub fn fd_apply_pwrite(fd: usize, offset: usize, data: &[u8]) -> bool {
    match snap(fd) {
        Some(file) => write_at(&mut file.state.lock().data, offset, data),
        None => false,
    }
}

/// Set descriptor `fd`'s snapshot to exactly `len` bytes (zero-filling growth),
/// mirroring an `ftruncate` the backing file has already accepted. The caller
/// first ran [`prepare_fd_write`] for `len`, so the capacity is reserved.
pub fn fd_set_len(fd: usize, len: usize) {
    if let Some(file) = snap(fd) {
        file.state.lock().data.resize(len, 0);
    }
}

/// Reposition a file descriptor (`whence`: 0=SET, 1=CUR, 2=END).
///
/// Returns `None` for an unknown `whence` or a signed position that would
/// overflow `i64` or be negative (Linux answers `-EINVAL` for both). The new
/// position may lie past end-of-file, as Linux allows for sparse writes; reads
/// there return zero bytes.
pub fn fd_seek(fd: usize, offset: i64, whence: u64) -> Option<u64> {
    let file = snap(fd)?;
    let mut state = file.state.lock();
    let base = match whence {
        0 => 0i64,
        1 => i64::try_from(state.offset).ok()?,
        2 => i64::try_from(state.data.len()).ok()?,
        _ => return None,
    };
    let new = base.checked_add(offset)?;
    if new < 0 {
        return None;
    }
    state.offset = new as usize;
    Some(new as u64)
}
