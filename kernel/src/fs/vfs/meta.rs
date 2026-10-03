//! VFS vocabulary: mode bits, metadata, ids, errors and the permission checks.

use super::attr::Times;
use crate::ipc::credentials;
use alloc::string::String;

/// Type and mode bits (Linux values); `mode` in [`Meta`] uses these.
#[cfg_attr(not(lazyos_tests), allow(dead_code))] // masked by tests/diagnostics
pub const S_IFMT: u16 = 0o170000;
/// Regular file.
pub const S_IFREG: u16 = 0o100000;
/// Directory.
pub const S_IFDIR: u16 = 0o040000;
/// Set-user-ID and set-group-ID: `chown` clears both on a regular file, and
/// `chmod` drops setgid for a caller outside the file's group (`setattr.rs`).
pub const S_ISUID: u16 = 0o4000;
pub const S_ISGID: u16 = 0o2000;
/// Sticky bit on a directory (see [`check_sticky`]).
pub const S_ISVTX: u16 = 0o1000;

/// Permission masks for [`check_access`], with the POSIX `R_OK`/`W_OK`/`X_OK`
/// values so `access(2)` can pass its mode straight through.
pub const READ: u8 = 4;
pub const WRITE: u8 = 2;
pub const EXECUTE: u8 = 1;

/// What kind of node an entry is. There is no symlink or device node yet: the
/// resolver is symlink-free and the ABI layer fabricates its device nodes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileKind {
    File,
    Dir,
}

/// Metadata for one node, as the trait and the caches carry it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Meta {
    /// Inode number within the mount. Not stable across mounts, so cache keys
    /// always pair it with the mount index.
    pub ino: u64,
    /// Full mode: type bits (`S_IF*`) plus `rwx` bits and suid/sgid/sticky.
    pub mode: u16,
    /// Owner uid, stamped from kernel credentials at creation.
    pub uid: u32,
    /// Owner gid, stamped from kernel credentials at creation.
    pub gid: u32,
    /// Size in bytes (directories report their serialized/entry size).
    pub size: u64,
    /// The node type, kept explicit so callers do not re-mask `mode`.
    pub kind: FileKind,
    /// Access, modification and change times (all zero where a backend keeps
    /// none, e.g. FAT and the fabricated ABI entries).
    pub times: Times,
}

/// Capacity figures for one mounted filesystem, the payload of `statfs(2)`.
/// Blocks are `block_size` bytes; `blocks_free` is what could still be
/// allocated (the VFS reserves nothing for root, so it is also the figure an
/// unprivileged caller gets).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StatFs {
    /// The Linux `f_type` magic that identifies the filesystem to `statfs`
    /// callers (`0xEF53` for ext2, `0x858458f6` for ramfs).
    pub magic: u32,
    pub block_size: u32,
    pub blocks: u64,
    pub blocks_free: u64,
    pub files: u64,
    pub files_free: u64,
    /// Longest file name a directory entry can hold.
    pub name_max: u32,
}

/// One directory entry: the name plus the target's inode and kind.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DirEntry {
    pub name: String,
    pub ino: u64,
    pub kind: FileKind,
}

/// A kernel-stamped `uid`/`gid` pair: the owner stamped on new nodes, or the
/// caller checked for access. Linux tasks keep their caps and label elsewhere
/// ([`crate::ipc::credentials`]); the VFS only needs the ids.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Id {
    pub uid: u32,
    pub gid: u32,
}

impl Id {
    /// The kernel/bring-up identity: uid 0, gid 0.
    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/diagnostics
    pub const ROOT: Id = Id { uid: 0, gid: 0 };

    #[cfg_attr(not(lazyos_tests), allow(dead_code))] // used by tests/diagnostics
    pub const fn new(uid: u32, gid: u32) -> Id {
        Id { uid, gid }
    }

    /// The current task's kernel-stamped credentials, read once per VFS call
    /// so a file cannot be reached with forged identity.
    pub fn current() -> Id {
        let cred = credentials::of(crate::task::current());
        Id {
            uid: cred.uid,
            gid: cred.gid,
        }
    }

    /// Root bypasses the permission bits (documented in [`check_access`]).
    pub const fn is_root(self) -> bool {
        self.uid == 0
    }
}

/// Errors shared by the VFS and every filesystem implementation. The Linux ABI
/// layer maps each variant to its errno; [`FsError::message`] is the friendly
/// kernel-side text (serial logs, test failures).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FsError {
    NotFound,
    Exists,
    NotDir,
    IsDir,
    NotEmpty,
    Access,
    /// The caller lacks the ownership an operation needs (`EPERM`), as
    /// opposed to a missing permission bit ([`FsError::Access`], `EACCES`).
    NotPermitted,
    ReadOnly,
    Invalid,
    NoSpace,
    NameTooLong,
    NotSupported,
}

impl FsError {
    /// Human-readable text for logs and diagnostics; not an errno.
    pub fn message(self) -> &'static str {
        match self {
            FsError::NotFound => "no such file or directory",
            FsError::Exists => "file already exists",
            FsError::NotDir => "not a directory",
            FsError::IsDir => "is a directory",
            FsError::NotEmpty => "directory not empty",
            FsError::Access => "permission denied",
            FsError::NotPermitted => "operation not permitted",
            FsError::ReadOnly => "read-only filesystem",
            FsError::Invalid => "invalid argument",
            FsError::NoSpace => "no space left on device",
            FsError::NameTooLong => "file name too long",
            FsError::NotSupported => "operation not supported",
        }
    }
}

/// Check the owner/group/other bits of `meta` against `mask` ([`READ`],
/// [`WRITE`], and/or [`EXECUTE`]).
///
/// The actor matches the owner bits when uids are equal, the group bits when
/// gids are equal (there are no supplementary groups yet), and the other bits
/// otherwise. Root bypasses the bits (`docs/security-model.md` section 4.1
/// grants that bypass to the kernel-init profile only, and uid 0 is that
/// profile until sessions land) with one exception, as on Linux: executing a
/// regular file needs at least one `x` bit even for root, or a `0644` file
/// could be run by init and by every root service. Directory search keeps the
/// full bypass.
pub fn check_access(meta: &Meta, id: Id, mask: u8) -> Result<(), FsError> {
    if mask == 0 {
        return Ok(());
    }
    if id.is_root() {
        return root_access(meta, mask);
    }
    let bits = if id.uid == meta.uid {
        (meta.mode >> 6) & 0o7
    } else if id.gid == meta.gid {
        (meta.mode >> 3) & 0o7
    } else {
        meta.mode & 0o7
    };
    if u16::from(mask) & bits == u16::from(mask) {
        Ok(())
    } else {
        Err(FsError::Access)
    }
}

/// Root's access: everything, except running a regular file that nobody may
/// execute (no `x` bit for owner, group or other).
fn root_access(meta: &Meta, mask: u8) -> Result<(), FsError> {
    let executes_file = mask & EXECUTE != 0 && meta.kind == FileKind::File;
    if executes_file && meta.mode & 0o111 == 0 {
        Err(FsError::Access)
    } else {
        Ok(())
    }
}

/// The sticky-bit rule for `unlink`/`rename` inside `dir` (mode `S_ISVTX`):
/// the actor must be root, the directory's owner, or the entry's owner.
pub fn check_sticky(dir: &Meta, entry: &Meta, id: Id) -> Result<(), FsError> {
    if dir.mode & S_ISVTX == 0 {
        return Ok(());
    }
    if id.is_root() || id.uid == dir.uid || id.uid == entry.uid {
        Ok(())
    } else {
        Err(FsError::Access)
    }
}
