//! Descriptors that read and write the persistent volume in place
//! ([`task::Fd::Vfs`]): `open`, `read`, `write`, `lseek`, `fstat` and the
//! positional variants.
//!
//! Every byte goes to the VFS at the descriptor's offset. There is no
//! snapshot, so a file is not bounded by the kernel heap, a second opener sees
//! the first one's writes, and the data is on the disk's side of `fsync`.
//! Permission was decided when the file was opened ([`OpenFile`]); the
//! handlers here only enforce the access mode the `open` asked for.

use alloc::vec::Vec;

use crate::fs::openfile::OpenFile;
use crate::fs::vfs::Meta;
use crate::task::{self, Fd};
use crate::user_ptr;

use super::errno::{err, fs_err, EBADF, EFAULT, EINVAL, ENOMEM};

/// Most bytes one `read` stages in kernel memory. A short read is legal, so a
/// larger request is served in pieces; this keeps the heap use per call
/// bounded. 1 MiB lets a large sequential read reach the filesystem as runs
/// long enough to skip the block cache (`ext2fs` `cache/range.rs`).
const READ_CHUNK: usize = 1 << 20;

/// Most bytes one `write` hands to the filesystem. Writes need no staging (the
/// user buffer is read in place), so this only bounds the time per call.
const WRITE_MAX: usize = 1 << 20;

/// `lseek` whence values.
const SEEK_SET: u64 = 0;
const SEEK_CUR: u64 = 1;
const SEEK_END: u64 = 2;

/// Open the regular file at `path` as a VFS-backed descriptor.
pub(super) fn open_vfs_fd(path: &str, readable: bool, writable: bool, append: bool) -> u64 {
    match OpenFile::open(path, readable, writable, append) {
        Ok(file) => super::fd::fd_result(task::fd_open(Fd::Vfs { file })),
        Err(error) => fs_err(error),
    }
}

/// Run `op` on the open file behind `fd`; `-EBADF` if `fd` is not a
/// VFS-backed descriptor. The description is cloned out of the table first, so
/// no table lock is held while the filesystem works.
pub(super) fn with_file(fd: u64, op: impl FnOnce(&OpenFile) -> u64) -> u64 {
    match task::fd_clone(fd as usize) {
        Some(Fd::Vfs { ref file }) => op(file),
        _ => err(EBADF),
    }
}

/// Current metadata of the open file behind `fd` (`fstat`, `statx`); `-EBADF`
/// if `fd` is not a VFS-backed descriptor.
pub(super) fn meta_of(fd: u64) -> Result<Meta, u64> {
    match task::fd_clone(fd as usize) {
        Some(Fd::Vfs { ref file }) => file.stat().map_err(fs_err),
        _ => Err(err(EBADF)),
    }
}

/// `read(2)`: from the descriptor offset, which then advances.
pub(super) fn read(file: &OpenFile, ptr: u64, len: u64) -> u64 {
    match read_at(file, file.offset(), ptr, len) {
        Ok(count) => {
            file.set_offset(file.offset() + count as u64);
            count as u64
        }
        Err(code) => code,
    }
}

/// `pread64(2)`: from `offset`, leaving the descriptor offset alone.
pub(super) fn pread(file: &OpenFile, ptr: u64, len: u64, offset: u64) -> u64 {
    match read_at(file, offset, ptr, len) {
        Ok(count) => count as u64,
        Err(code) => code,
    }
}

/// Stage up to [`READ_CHUNK`] bytes from `offset` and copy them out. The bytes
/// go through a kernel buffer so a bad user buffer is `-EFAULT` with nothing
/// lost, and the offset only moves once the copy succeeded.
fn read_at(file: &OpenFile, offset: u64, ptr: u64, len: u64) -> Result<usize, u64> {
    if !file.readable() {
        return Err(err(EBADF));
    }
    let want = (len as usize).min(READ_CHUNK);
    if want == 0 {
        return Ok(0);
    }
    let mut buf = Vec::new();
    if buf.try_reserve_exact(want).is_err() {
        return Err(err(ENOMEM));
    }
    buf.resize(want, 0);
    let count = file.read_at(offset, &mut buf).map_err(fs_err)?;
    user_ptr::try_copy_to(ptr, &buf[..count]).map_err(|_| err(EFAULT))?;
    Ok(count)
}

/// `write(2)`: at the descriptor offset (or EOF for `O_APPEND`); the offset
/// ends up just past the bytes written.
pub(super) fn write(file: &OpenFile, ptr: u64, len: u64) -> u64 {
    match write_at(file, file.write_position(), ptr, len) {
        Ok((at, count)) => {
            file.set_offset(at + count as u64);
            count as u64
        }
        Err(code) => code,
    }
}

/// `pwrite64(2)`: at `offset` (Linux appends instead on an `O_APPEND` file),
/// leaving the descriptor offset alone.
pub(super) fn pwrite(file: &OpenFile, ptr: u64, len: u64, offset: u64) -> u64 {
    let at = if file.append() {
        file.write_position()
    } else {
        Ok(offset)
    };
    match write_at(file, at, ptr, len) {
        Ok((_, count)) => count as u64,
        Err(code) => code,
    }
}

/// Write `len` user bytes at `at`; returns where they landed and how many.
fn write_at(
    file: &OpenFile,
    at: Result<u64, crate::fs::vfs::FsError>,
    ptr: u64,
    len: u64,
) -> Result<(u64, usize), u64> {
    if !file.writable() {
        return Err(err(EBADF));
    }
    let at = at.map_err(fs_err)?;
    let want = (len as usize).min(WRITE_MAX);
    if want == 0 {
        return Ok((at, 0));
    }
    let bytes = user_ptr::try_bytes(ptr, want).map_err(|_| err(EFAULT))?;
    let count = file.write_at(at, bytes).map_err(fs_err)?;
    Ok((at, count))
}

/// `lseek(2)`. `SEEK_END` asks the filesystem for the current size, so it sees
/// other descriptors' writes. A position past the end is legal (a later write
/// leaves a hole); a negative one is `-EINVAL`.
pub(super) fn seek(file: &OpenFile, offset: i64, whence: u64) -> u64 {
    let base = match whence {
        SEEK_SET => 0,
        SEEK_CUR => file.offset(),
        SEEK_END => match file.stat() {
            Ok(meta) => meta.size,
            Err(error) => return fs_err(error),
        },
        _ => return err(EINVAL),
    };
    let target = i64::try_from(base)
        .ok()
        .and_then(|base| base.checked_add(offset))
        .filter(|target| *target >= 0);
    match target {
        Some(target) => {
            file.set_offset(target as u64);
            target as u64
        }
        None => err(EINVAL),
    }
}
