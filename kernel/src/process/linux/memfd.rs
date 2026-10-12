//! `memfd_create` (319) and the file operations on its descriptor
//! ([`task::Fd::Memfd`]): `read`, `write`, `pread64`, `pwrite64`, `lseek`,
//! `ftruncate` and `fstat`.
//!
//! The file is a [`Memfd`]: sparse, frame-backed and shared by every
//! descriptor that duplicates it, which is what lets `mmap(MAP_SHARED)` hand
//! the same pages to another process ([`super::mmapfile`]). It shows in
//! `/proc/self/fd/N` as `/memfd:<name> (deleted)`, as on Linux.
//!
//! `MFD_ALLOW_SEALING` is accepted and does nothing: seals (`F_ADD_SEALS`)
//! are not implemented, so `fcntl` answers `EINVAL` for them.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::mem::memfd::{Memfd, MemfdError, NAME_MAX};
use crate::task::{self, Fd};
use crate::user_ptr::{self, CStrError};

use super::errno::{err, EBADF, EFAULT, EFBIG, EINVAL, EMFILE, ENOMEM, ENOSPC};
use super::flags::S_IFREG;
use super::stat::Attrs;

const MFD_CLOEXEC: u64 = 1;
const MFD_ALLOW_SEALING: u64 = 2;

/// Bytes one `read` or `write` moves through the kernel at a time. A longer
/// request is a short transfer, which is legal for a regular file.
const CHUNK: usize = 1 << 20;

/// `lseek` whence values.
const SEEK_SET: u64 = 0;
const SEEK_CUR: u64 = 1;
const SEEK_END: u64 = 2;

/// The errno for a refused memfd operation.
pub(super) fn mem_err(error: MemfdError) -> u64 {
    err(match error {
        MemfdError::NoSpace => ENOSPC,
        MemfdError::NoMemory => ENOMEM,
        MemfdError::TooBig => EFBIG,
    })
}

/// `memfd_create(name, flags)`: a new empty memory file owned by the caller.
pub(super) fn sys_memfd_create(name: u64, flags: u64) -> u64 {
    if flags & !(MFD_CLOEXEC | MFD_ALLOW_SEALING) != 0 {
        return err(EINVAL);
    }
    let name = match user_ptr::try_cstr(name, NAME_MAX + 1) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(CStrError::Fault) => return err(EFAULT),
        Err(CStrError::Unterminated) => return err(EINVAL),
    };
    let uid = crate::ipc::credentials::of(task::current()).uid;
    let Some(mem) = Memfd::new(&name, uid) else {
        return err(ENOMEM);
    };
    let Some(fd) = task::fd_open(Fd::Memfd { mem }) else {
        return err(EMFILE);
    };
    if flags & MFD_CLOEXEC != 0 {
        task::fd_set_cloexec(fd, true);
    }
    fd as u64
}

/// The memory file behind `fd`, cloned out of the table so no lock is held
/// while it works; `-EBADF` for any other descriptor.
pub(super) fn with_memfd(fd: u64, op: impl FnOnce(&Arc<Memfd>) -> u64) -> u64 {
    match task::fd_clone(fd as usize) {
        Some(Fd::Memfd { ref mem }) => op(mem),
        _ => err(EBADF),
    }
}

/// `read(2)`: from the descriptor offset, which then advances.
pub(super) fn read(mem: &Memfd, ptr: u64, len: u64) -> u64 {
    let got = read_at(mem, mem.offset(), ptr, len);
    if (got as i64) > 0 {
        mem.set_offset(mem.offset() + got);
    }
    got
}

/// `pread64(2)`: from `offset`, leaving the descriptor offset alone.
pub(super) fn pread(mem: &Memfd, ptr: u64, len: u64, offset: u64) -> u64 {
    read_at(mem, offset, ptr, len)
}

fn read_at(mem: &Memfd, offset: u64, ptr: u64, len: u64) -> u64 {
    let want = usize::try_from(len).unwrap_or(usize::MAX).min(CHUNK);
    let mut buf = Vec::new();
    if buf.try_reserve_exact(want).is_err() {
        return err(ENOMEM);
    }
    buf.resize(want, 0);
    let count = mem.read_at(offset, &mut buf);
    match user_ptr::try_copy_to(ptr, &buf[..count]) {
        Ok(()) => count as u64,
        Err(_) => err(EFAULT),
    }
}

/// `write(2)`: at the descriptor offset, which ends up past the bytes.
pub(super) fn write(mem: &Memfd, ptr: u64, len: u64) -> u64 {
    let wrote = write_at(mem, mem.offset(), ptr, len);
    if (wrote as i64) > 0 {
        mem.set_offset(mem.offset() + wrote);
    }
    wrote
}

/// `pwrite64(2)`: at `offset`, leaving the descriptor offset alone.
pub(super) fn pwrite(mem: &Memfd, ptr: u64, len: u64, offset: u64) -> u64 {
    write_at(mem, offset, ptr, len)
}

fn write_at(mem: &Memfd, offset: u64, ptr: u64, len: u64) -> u64 {
    let want = usize::try_from(len).unwrap_or(usize::MAX).min(CHUNK);
    if want == 0 {
        return 0;
    }
    let bytes = match super::vfsfd::stage(ptr, want) {
        Ok(bytes) => bytes,
        Err(code) => return code,
    };
    match mem.write_at(offset, &bytes) {
        Ok(count) => count as u64,
        Err(error) => mem_err(error),
    }
}

/// `lseek(2)`: `SEEK_END` is the current size. A position past the end is
/// legal (a later write leaves a hole); a negative one is `-EINVAL`.
pub(super) fn seek(mem: &Memfd, offset: i64, whence: u64) -> u64 {
    let base = match whence {
        SEEK_SET => 0,
        SEEK_CUR => mem.offset(),
        SEEK_END => mem.size(),
        _ => return err(EINVAL),
    };
    let target = i64::try_from(base)
        .ok()
        .and_then(|base| base.checked_add(offset))
        .filter(|target| *target >= 0);
    match target {
        Some(target) => {
            mem.set_offset(target as u64);
            target as u64
        }
        None => err(EINVAL),
    }
}

/// `ftruncate(2)`: the new size.
pub(super) fn truncate(mem: &Memfd, length: u64) -> u64 {
    match mem.set_size(length) {
        Ok(()) => 0,
        Err(error) => mem_err(error),
    }
}

/// `fstat` of a memory file: a regular file of mode 0777, as Linux reports.
pub(super) fn attrs(fd: u64) -> Result<Attrs, u64> {
    match task::fd_clone(fd as usize) {
        Some(Fd::Memfd { ref mem }) => Ok(Attrs {
            mode: S_IFREG | 0o777,
            size: mem.size(),
            ino: (Arc::as_ptr(mem) as u64 >> 4) & 0xff_ffff,
            uid: mem.uid(),
            gid: 0,
            times: Default::default(),
        }),
        _ => Err(err(EBADF)),
    }
}

/// The `/proc/self/fd/N` link text.
pub(super) fn link_text(mem: &Memfd) -> String {
    alloc::format!("/memfd:{} (deleted)", mem.name())
}
