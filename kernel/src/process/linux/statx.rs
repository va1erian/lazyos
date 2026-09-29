//! `statx(2)`: the extensible `stat`, mapped onto the attributes the plain
//! stat family already gathers ([`super::stat`]).
//!
//! LazyOS keeps no timestamps and has no symlinks, automounts or cached
//! attributes, so the reply says exactly that through `stx_mask`: the time
//! fields (birth time included) are not claimed, `AT_SYMLINK_NOFOLLOW` and
//! `AT_NO_AUTOMOUNT` change nothing, and the `AT_STATX_*` sync hints are
//! accepted and ignored because every answer is already current.

use alloc::string::String;

use crate::user_ptr::{self, CStrError};

use super::errno::{err, EFAULT, EINVAL, ENAMETOOLONG, ENOENT};
use super::path::{resolve_at, AT_FDCWD};
use super::stat::{fd_attrs, path_attrs, Attrs};

// `flags` bits (Linux x86_64 values).
const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
const AT_NO_AUTOMOUNT: u64 = 0x800;
const AT_EMPTY_PATH: u64 = 0x1000;
/// The two-bit sync-type field; both bits set at once is not a valid choice.
const AT_STATX_SYNC_TYPE: u64 = 0x6000;
const KNOWN_FLAGS: u64 = AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT | AT_EMPTY_PATH | AT_STATX_SYNC_TYPE;

// `mask` / `stx_mask` bits.
const STATX_TYPE: u32 = 0x1;
const STATX_MODE: u32 = 0x2;
const STATX_NLINK: u32 = 0x4;
const STATX_UID: u32 = 0x8;
const STATX_GID: u32 = 0x10;
const STATX_INO: u32 = 0x100;
const STATX_SIZE: u32 = 0x200;
const STATX_BLOCKS: u32 = 0x400;
/// Reserved for future use: a caller that sets it is refused.
const STATX_RESERVED: u32 = 0x8000_0000;
/// What every reply carries. Answering more than was asked is allowed; the
/// caller reads `stx_mask` to see what it got.
const STATX_SUPPORTED: u32 = STATX_TYPE
    | STATX_MODE
    | STATX_NLINK
    | STATX_UID
    | STATX_GID
    | STATX_INO
    | STATX_SIZE
    | STATX_BLOCKS;

/// Longest path accepted, as for the other path syscalls.
const PATH_MAX: usize = 4096;
/// Size of `struct statx`; the tail past the fields below is reserved zeros.
const STATX_SIZE_BYTES: usize = 256;

/// `statx(dirfd, pathname, flags, mask, statxbuf)`.
pub(super) fn sys_statx(dirfd: u64, path: u64, flags: u64, mask: u64, buf: u64) -> u64 {
    // `flags` and `mask` are C `int`/`unsigned`: only their low 32 bits count.
    let (flags, mask) = (flags & 0xffff_ffff, mask & 0xffff_ffff);
    if flags & !KNOWN_FLAGS != 0 || flags & AT_STATX_SYNC_TYPE == AT_STATX_SYNC_TYPE {
        return err(EINVAL);
    }
    if mask as u32 & STATX_RESERVED != 0 {
        return err(EINVAL);
    }
    let attrs = match lookup(dirfd, path, flags) {
        Ok(attrs) => attrs,
        Err(code) => return code,
    };
    // Nothing is written unless the whole lookup succeeded.
    match user_ptr::try_copy_to(buf, &encode(&attrs)) {
        Ok(()) => 0,
        Err(_) => err(EFAULT),
    }
}

/// The attributes `(dirfd, path)` names. An empty path names `dirfd` itself
/// when `AT_EMPTY_PATH` is set (and the current directory, the root, for
/// `AT_FDCWD`).
fn lookup(dirfd: u64, path: u64, flags: u64) -> Result<Attrs, u64> {
    let path = read_path(path)?;
    if path.is_empty() {
        if flags & AT_EMPTY_PATH == 0 {
            return Err(err(ENOENT));
        }
        return if dirfd == AT_FDCWD {
            path_attrs("/")
        } else {
            fd_attrs(dirfd)
        };
    }
    // `resolve_at` reports a bare errno; the ABI wants it negated.
    path_attrs(&resolve_at(dirfd, &path).map_err(err)?)
}

/// Read the user path: unreadable memory is `-EFAULT`, a path with no NUL in
/// `PATH_MAX` bytes `-ENAMETOOLONG`, and one that is not UTF-8 names nothing
/// (`-ENOENT`), as no file here can have such a name.
fn read_path(ptr: u64) -> Result<String, u64> {
    match user_ptr::try_cstr(ptr, PATH_MAX) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| err(ENOENT)),
        Err(CStrError::Fault) => Err(err(EFAULT)),
        Err(CStrError::Unterminated) => Err(err(ENAMETOOLONG)),
    }
}

/// Lay `attrs` out as a `struct statx`.
fn encode(attrs: &Attrs) -> [u8; STATX_SIZE_BYTES] {
    let mut out = [0u8; STATX_SIZE_BYTES];
    let mut put = |at: usize, bytes: &[u8]| out[at..at + bytes.len()].copy_from_slice(bytes);
    put(0, &STATX_SUPPORTED.to_le_bytes()); // stx_mask
    put(4, &4096u32.to_le_bytes()); // stx_blksize
    put(16, &1u32.to_le_bytes()); // stx_nlink
    put(20, &attrs.uid.to_le_bytes()); // stx_uid
    put(24, &attrs.gid.to_le_bytes()); // stx_gid
    put(28, &(attrs.mode as u16).to_le_bytes()); // stx_mode
    put(32, &attrs.ino.to_le_bytes()); // stx_ino
    put(40, &attrs.size.to_le_bytes()); // stx_size
    put(48, &attrs.size.div_ceil(512).to_le_bytes()); // stx_blocks
    out
}
