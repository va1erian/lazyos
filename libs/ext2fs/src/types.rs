//! The vocabulary the public API speaks: modes, owners, metadata and the
//! attribute change set. The kernel converts these to its VFS types.

use alloc::string::String;

/// Type bits of an `i_mode` (Linux values).
pub const S_IFMT: u16 = 0o170000;
/// Regular file.
pub const S_IFREG: u16 = 0o100000;
/// Directory.
pub const S_IFDIR: u16 = 0o040000;

/// What kind of node an entry is. Symlinks and device nodes are not
/// representable: an inode of another type answers `NotSupported`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileKind {
    File,
    Dir,
}

/// A `uid`/`gid` pair for a new node. Only 16 bits of each are stored.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Owner {
    pub uid: u32,
    pub gid: u32,
}

impl Owner {
    pub const ROOT: Owner = Owner { uid: 0, gid: 0 };
}

/// The three timestamps of a node, whole seconds since the Unix epoch.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Times {
    pub atime: i64,
    pub mtime: i64,
    pub ctime: i64,
}

/// One attribute change. A `Some` field overwrites the stored value, a `None`
/// field is left alone; the whole change is validated before anything is
/// written.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct AttrChange {
    /// Permission bits (`0o7777`); the node's type bits never change.
    pub mode: Option<u16>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub atime: Option<i64>,
    pub mtime: Option<i64>,
    pub ctime: Option<i64>,
}

/// Metadata for one node.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct InodeMeta {
    pub ino: u64,
    /// Type bits (`S_IF*`) plus permission bits.
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    /// Bytes for a file; the directory's block bytes for a directory.
    pub size: u64,
    pub kind: FileKind,
    pub times: Times,
}

/// Capacity figures for the volume (the payload of `statfs`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FsStats {
    /// The Linux `f_type` magic (`0xEF53`).
    pub magic: u32,
    pub block_size: u32,
    pub blocks: u64,
    pub blocks_free: u64,
    pub files: u64,
    pub files_free: u64,
    pub name_max: u32,
}

/// One directory entry: the name plus the target's inode and kind.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DirEntry {
    pub name: String,
    pub ino: u64,
    pub kind: FileKind,
}
