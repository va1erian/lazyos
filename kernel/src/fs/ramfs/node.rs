//! One ramfs node and its attributes. Split from `ramfs.rs` to keep that file
//! under the size limit; the tree operations stay there.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::vfs::{self, FileKind, Id, Meta, SetAttr, Times, S_IFDIR, S_IFREG};

/// One name in a directory. A file has as many entries as it has names
/// (hard links); a directory has exactly one.
pub(super) struct Entry {
    pub(super) name: String,
    pub(super) ino: u64,
    /// Who pays for this name when it is not the file's first one: a hard
    /// link costs a node of the filesystem's cap, charged to the caller who
    /// made it (`links.rs`). `None` on the one entry the node's own charge
    /// covers.
    pub(super) extra: Option<u32>,
}

/// One node: a file with contents, or a directory with children.
pub(super) struct Node {
    pub(super) kind: FileKind,
    /// `st_nlink`: a file's names; a directory's `.`, its entry in the
    /// parent and each child directory's `..`.
    pub(super) links: u32,
    /// Permission bits only (type bits are added by [`Node::meta`]).
    mode: u16,
    uid: u32,
    gid: u32,
    times: Times,
    pub(super) data: Vec<u8>,
    pub(super) children: Vec<Entry>,
}

impl Node {
    /// A fresh, empty node owned by `owner`, all three times stamped now.
    pub(super) fn new(kind: FileKind, mode: u16, owner: Id) -> Node {
        Node {
            kind,
            links: match kind {
                FileKind::File => 1,
                FileKind::Dir => 2,
            },
            mode: mode & 0o7777,
            uid: owner.uid,
            gid: owner.gid,
            times: Times::all(vfs::now()),
            data: Vec::new(),
            children: Vec::new(),
        }
    }

    /// The owning uid, which pays for the node and its bytes (`space.rs`).
    pub(super) fn owner(&self) -> u32 {
        self.uid
    }

    pub(super) fn meta(&self, ino: u64) -> Meta {
        let kind_bits = match self.kind {
            FileKind::File => S_IFREG,
            FileKind::Dir => S_IFDIR,
        };
        Meta {
            ino,
            mode: kind_bits | self.mode,
            uid: self.uid,
            gid: self.gid,
            size: self.data.len() as u64,
            kind: self.kind,
            times: self.times,
            nlink: self.links,
        }
    }

    /// The child entry called `name`, if any.
    pub(super) fn entry(&self, name: &str) -> Option<&Entry> {
        self.children.iter().find(|entry| entry.name == name)
    }

    /// The inode change time moves (a name came or went, an attribute changed).
    pub(super) fn touch_ctime(&mut self) {
        self.times.ctime = vfs::now();
    }

    /// The contents changed: `mtime` and `ctime` move.
    pub(super) fn touch(&mut self) {
        let now = vfs::now();
        self.times.mtime = now;
        self.times.ctime = now;
    }

    /// Apply an authorized attribute change. Nothing here can fail, so the
    /// all-or-nothing rule of [`vfs::Filesystem::setattr`] holds trivially.
    pub(super) fn set_attr(&mut self, attr: &SetAttr) {
        if let Some(mode) = attr.mode {
            self.mode = mode & 0o7777;
        }
        if let Some(uid) = attr.uid {
            self.uid = uid;
        }
        if let Some(gid) = attr.gid {
            self.gid = gid;
        }
        attr.apply_times(&mut self.times);
    }
}
