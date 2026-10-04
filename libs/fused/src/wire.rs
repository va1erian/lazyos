//! Syscall 35 and its records.
//!
//! ```text
//!   rdi = op, rsi/rdx/r10/r8 = a1..a4; every return is a value or -errno
//!
//!   op 0 REGISTER(name, len, flags)  mount a new provider at /mnt/<name>
//!                                    (flags: FLAG_RO, FLAG_NOEXEC) -> id
//!   op 1 NEXT(id, req, data, cap | deadline << 32)
//!                                    wait until `deadline` (absolute ticks,
//!                                    capped at now + 1 s) for a request;
//!                                    writes req -> [u64; REQUEST_WORDS] and
//!                                    its payload to `data` (cap >= its
//!                                    payload) -> 1, or 0 when none came
//!   op 2 REPLY(id, reply, data)      answer the request reply[0] names; a
//!                                    reply carrying data passes data_len
//!                                    bytes at `data`
//!   op 3 UNREGISTER(id)              unmount: pending and later requests
//!                                    fail with EIO
//! ```
//!
//! A request names its file by path (relative to the mount root, `""` is the
//! root) or, with [`FLAG_NODE`] in its op word, by the `(ino, generation)` a
//! `LOOKUP` reply gave it: an open file is read and written by node, so a
//! rename does not lose it and the kernel need not resolve the path again.

/// The syscall number.
pub const SYS_FUSE: u64 = 35;

/// Syscall operations (`rdi`).
pub mod sys_op {
    pub const REGISTER: u64 = 0;
    pub const NEXT: u64 = 1;
    pub const REPLY: u64 = 2;
    pub const UNREGISTER: u64 = 3;
}

/// REGISTER flag: the mount is read-only (the VFS refuses writes first).
pub const FLAG_RO: u64 = 1;
/// REGISTER flag: nothing on the mount is executed.
pub const FLAG_NOEXEC: u64 = 2;
/// Every REGISTER flag.
pub const FLAGS_ALL: u64 = FLAG_RO | FLAG_NOEXEC;

/// The largest data one request or reply carries (a longer read or write is
/// split into requests of at most this).
pub const MAX_DATA: usize = 64 * 1024;
/// The longest path a request carries, bytes.
pub const MAX_PATH: usize = 4096;
/// The longest name of one directory entry, bytes.
pub const MAX_NAME: usize = 255;
/// The longest mount name (`/mnt/<name>`), bytes.
pub const MAX_MOUNT_NAME: usize = 64;
/// The largest request payload: a path and a write's data, or two paths.
pub const MAX_PAYLOAD: usize = MAX_PATH + MAX_DATA;

/// Request operations (the low 16 bits of the op word).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// Metadata of a path (or of a node: `stat` on an open file).
    Lookup = 1,
    /// `len` bytes at `offset`; the reply carries them.
    Read = 2,
    /// The payload's data at `offset`; the reply's `count` is what was written.
    Write = 3,
    /// Truncate or zero-extend to `offset` bytes.
    Truncate = 4,
    /// Apply the payload's [`crate::payload::SetAttrRecord`].
    SetAttr = 5,
    /// A regular file with `mode`, owned by `uid`/`gid`.
    Create = 6,
    /// A directory with `mode`, owned by `uid`/`gid`.
    Mkdir = 7,
    Unlink = 8,
    Rmdir = 9,
    /// Rename the path to the payload's second path.
    Rename = 10,
    /// Entries from index `offset` on, as many as fit in `len` bytes.
    ReadDir = 11,
    /// Make everything written so far durable.
    Flush = 12,
    /// Capacity figures ([`crate::payload::StatFsRecord`]).
    StatFs = 13,
}

impl Op {
    pub fn from_code(code: u64) -> Option<Op> {
        Some(match code {
            1 => Op::Lookup,
            2 => Op::Read,
            3 => Op::Write,
            4 => Op::Truncate,
            5 => Op::SetAttr,
            6 => Op::Create,
            7 => Op::Mkdir,
            8 => Op::Unlink,
            9 => Op::Rmdir,
            10 => Op::Rename,
            11 => Op::ReadDir,
            12 => Op::Flush,
            13 => Op::StatFs,
            _ => return None,
        })
    }

    /// Whether the request's payload carries `len` bytes after the path.
    pub fn sends_data(self) -> bool {
        matches!(self, Op::Write | Op::Rename | Op::SetAttr)
    }

    /// Whether the op may name a node instead of a path.
    pub fn takes_node(self) -> bool {
        matches!(self, Op::Lookup | Op::Read | Op::Write | Op::Truncate)
    }
}

/// Op-word flag: the request names `(ino, generation)`, not a path.
pub const FLAG_NODE: u64 = 1 << 16;

/// Words in a request record.
pub const REQUEST_WORDS: usize = 10;
/// Words in a reply record.
pub const REPLY_WORDS: usize = 13;

/// One request, as `NEXT` hands it to the provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub tag: u64,
    /// An [`Op`] code, with [`FLAG_NODE`] when it names a node.
    pub op: u64,
    pub ino: u64,
    pub generation: u64,
    /// Read/write offset, truncate size, or first directory index.
    pub offset: u64,
    /// Bytes to read or written, a readdir's room, a rename target's or a
    /// setattr record's length.
    pub len: u64,
    /// Bytes of path at the start of the payload.
    pub path_len: u64,
    /// Create/mkdir: the new node's mode and owner.
    pub mode: u64,
    pub uid: u64,
    pub gid: u64,
}

impl Request {
    /// All zero (a `const` stand-in for `Default`).
    pub const EMPTY: Request = Request {
        tag: 0,
        op: 0,
        ino: 0,
        generation: 0,
        offset: 0,
        len: 0,
        path_len: 0,
        mode: 0,
        uid: 0,
        gid: 0,
    };

    pub fn to_words(&self) -> [u64; REQUEST_WORDS] {
        [
            self.tag,
            self.op,
            self.ino,
            self.generation,
            self.offset,
            self.len,
            self.path_len,
            self.mode,
            self.uid,
            self.gid,
        ]
    }

    pub fn from_words(words: &[u64; REQUEST_WORDS]) -> Request {
        let [tag, op, ino, generation, offset, len, path_len, mode, uid, gid] = *words;
        Request {
            tag,
            op,
            ino,
            generation,
            offset,
            len,
            path_len,
            mode,
            uid,
            gid,
        }
    }

    /// The operation, `None` for a code this side does not know.
    pub fn operation(&self) -> Option<Op> {
        Op::from_code(self.op & 0xFFFF)
    }

    pub fn by_node(&self) -> bool {
        self.op & FLAG_NODE != 0
    }

    /// Bytes of payload this request carries, `None` when its lengths are
    /// out of range (a daemon must refuse such a request).
    pub fn payload_len(&self) -> Option<usize> {
        let path = usize::try_from(self.path_len).ok().filter(|&n| n <= MAX_PATH)?;
        let data = match self.operation()? {
            op if op.sends_data() => usize::try_from(self.len).ok().filter(|&n| n <= MAX_DATA)?,
            _ => 0,
        };
        Some(path + data)
    }
}

/// A node's attributes, as a reply carries them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attr {
    pub ino: u64,
    pub generation: u64,
    /// Type bits ([`S_IFREG`] or [`S_IFDIR`]) and permission bits.
    pub mode: u64,
    pub uid: u64,
    pub gid: u64,
    pub size: u64,
    pub atime: i64,
    pub mtime: i64,
    pub ctime: i64,
}

/// Regular file type bits (Linux values).
pub const S_IFREG: u64 = 0o100000;
/// Directory type bits.
pub const S_IFDIR: u64 = 0o040000;
/// The type-bit mask.
pub const S_IFMT: u64 = 0o170000;

impl Attr {
    pub fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }

    pub fn is_file(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }
}

/// One reply, as `REPLY` hands it to the kernel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reply {
    pub tag: u64,
    /// 0, or a positive Linux errno ([`errno`]).
    pub status: u64,
    /// Bytes read or written, or directory entries in the payload.
    pub count: u64,
    /// Bytes of payload passed beside the record.
    pub data_len: u64,
    pub attr: Attr,
}

impl Reply {
    /// All zero (a `const` stand-in for `Default`).
    pub const EMPTY: Reply = Reply {
        tag: 0,
        status: 0,
        count: 0,
        data_len: 0,
        attr: Attr {
            ino: 0,
            generation: 0,
            mode: 0,
            uid: 0,
            gid: 0,
            size: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        },
    };

    pub fn to_words(&self) -> [u64; REPLY_WORDS] {
        let a = &self.attr;
        [
            self.tag,
            self.status,
            self.count,
            self.data_len,
            a.ino,
            a.generation,
            a.mode,
            a.uid,
            a.gid,
            a.size,
            a.atime as u64,
            a.mtime as u64,
            a.ctime as u64,
        ]
    }

    pub fn from_words(words: &[u64; REPLY_WORDS]) -> Reply {
        let [tag, status, count, data_len, ino, generation, mode, uid, gid, size, atime, mtime, ctime] =
            *words;
        Reply {
            tag,
            status,
            count,
            data_len,
            attr: Attr {
                ino,
                generation,
                mode,
                uid,
                gid,
                size,
                atime: atime as i64,
                mtime: mtime as i64,
                ctime: ctime as i64,
            },
        }
    }
}

/// The errnos a reply's `status` uses (Linux values). Any other nonzero code
/// is an I/O error to the kernel.
pub mod errno {
    pub const EPERM: u64 = 1;
    pub const ENOENT: u64 = 2;
    pub const EIO: u64 = 5;
    pub const EACCES: u64 = 13;
    pub const EEXIST: u64 = 17;
    pub const ENOTDIR: u64 = 20;
    pub const EISDIR: u64 = 21;
    pub const EINVAL: u64 = 22;
    pub const ENOSPC: u64 = 28;
    pub const EROFS: u64 = 30;
    pub const ENAMETOOLONG: u64 = 36;
    pub const ENOSYS: u64 = 38;
    pub const ENOTEMPTY: u64 = 39;
    pub const EOPNOTSUPP: u64 = 95;
    /// A node handle whose file is gone.
    pub const ESTALE: u64 = 116;
}

/// Whether `name` may name a mount (`/mnt/<name>`): 1..=64 bytes of ASCII
/// letters, digits, `-`, `_` and `.`, and not `.` or `..`.
pub fn valid_mount_name(name: &[u8]) -> bool {
    (1..=MAX_MOUNT_NAME).contains(&name.len())
        && name != b"."
        && name != b".."
        && name
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}
