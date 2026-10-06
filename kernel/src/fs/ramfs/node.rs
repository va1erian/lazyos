//! One ramfs node and its attributes. Split from `ramfs.rs` to keep that file
//! under the size limit; the tree operations stay there.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::vfs::{self, FileKind, Id, Meta, SetAttr, Times, S_IFDIR, S_IFREG};

/// One node: a file with contents, or a directory with children.
pub(super) struct Node {
    pub(super) name: String,
    pub(super) kind: FileKind,
    /// Permission bits only (type bits are added by [`Node::meta`]).
    mode: u16,
    uid: u32,
    gid: u32,
    times: Times,
    pub(super) data: Vec<u8>,
    pub(super) children: Vec<u64>,
}

impl Node {
    /// A fresh, empty node owned by `owner`, all three times stamped now.
    pub(super) fn new(name: String, kind: FileKind, mode: u16, owner: Id) -> Node {
        Node {
            name,
            kind,
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
        }
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
