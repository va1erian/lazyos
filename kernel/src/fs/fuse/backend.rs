//! [`FuseFs`]: the [`Filesystem`] a provider's mount presents. Every call
//! becomes one request (or, for a long read, write or directory, a run of
//! them), and every reply is checked before it becomes VFS metadata: a
//! node type other than file or directory, an id out of range, a count
//! larger than asked for, or a malformed directory payload is an I/O error,
//! never a guess.

use alloc::vec;
use alloc::vec::Vec;

use fused::payload::{self, set, SetAttrRecord, StatFsRecord};
use fused::wire::{errno, Attr, Op, Reply, Request, FLAG_NODE, MAX_DATA, MAX_PATH};

use super::channel::transact;
use crate::fs::vfs::{
    DirEntry, FileKind, Filesystem, FsError, Id, Meta, NodeId, SetAttr, StatFs, Times,
};

/// The most entries one directory may list (a daemon that never ends a
/// listing cannot make the kernel allocate without bound).
pub const MAX_DIR_ENTRIES: usize = 1 << 16;

/// A provider's mount: slot `index` while it holds registration `epoch`.
pub struct FuseFs {
    index: usize,
    epoch: u64,
}

/// What a request names.
#[derive(Clone, Copy)]
enum Target<'a> {
    Path(&'a str),
    Node(NodeId),
}

impl FuseFs {
    pub(super) fn new(index: usize, epoch: u64) -> FuseFs {
        FuseFs { index, epoch }
    }

    /// One request: `op` on `target`, with `data` after the path, reply
    /// data into `out`. A nonzero status is the daemon's error.
    fn call(
        &self,
        op: Op,
        target: Target,
        mut request: Request,
        data: &[u8],
        out: &mut [u8],
    ) -> Result<(Reply, usize), FsError> {
        request.op = op as u64;
        let path: &[u8] = match target {
            Target::Path(path) => path.as_bytes(),
            Target::Node(node) => {
                request.op |= FLAG_NODE;
                request.ino = node.ino;
                request.generation = u64::from(node.generation);
                &[]
            }
        };
        if path.len() > MAX_PATH {
            return Err(FsError::NameTooLong);
        }
        request.path_len = path.len() as u64;
        if op.sends_data() {
            request.len = data.len() as u64;
        }
        let (reply, len) = transact(self.index, self.epoch, request, &[path, data], out)?;
        match reply.status {
            0 => Ok((reply, len)),
            code => Err(error_of(code)),
        }
    }

    /// A request answered by a node's attributes.
    fn attr_call(&self, op: Op, target: Target, request: Request, data: &[u8]) -> Result<Meta, FsError> {
        let (reply, _) = self.call(op, target, request, data, &mut [])?;
        meta_of(&reply.attr)
    }

    fn lookup_target(&self, target: Target) -> Result<Meta, FsError> {
        self.attr_call(Op::Lookup, target, Request::default(), &[])
    }

    fn read_target(&self, target: Target, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        let mut done = 0;
        for chunk in buf.chunks_mut(MAX_DATA) {
            let at = offset.checked_add(done as u64).ok_or(FsError::Invalid)?;
            let request = Request {
                offset: at,
                len: chunk.len() as u64,
                ..Request::default()
            };
            let want = chunk.len();
            let (reply, len) = self.call(Op::Read, target, request, &[], chunk)?;
            if reply.count as usize != len || len > want {
                return Err(FsError::Io);
            }
            done += len;
            if len < want {
                break;
            }
        }
        Ok(done)
    }

    /// Write in chunks; a short or failed chunk ends the write with what the
    /// earlier chunks wrote (an error only when nothing was).
    fn write_target(&self, target: Target, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        let mut done = 0;
        for chunk in data.chunks(MAX_DATA) {
            let at = offset.checked_add(done as u64).ok_or(FsError::Invalid)?;
            let request = Request {
                offset: at,
                ..Request::default()
            };
            let written = match self.call(Op::Write, target, request, chunk, &mut []) {
                Ok((reply, _)) if reply.count as usize <= chunk.len() => reply.count as usize,
                Ok(_) => return Err(FsError::Io),
                Err(error) if done == 0 => return Err(error),
                Err(_) => break,
            };
            done += written;
            if written < chunk.len() {
                break;
            }
        }
        Ok(done)
    }

    fn truncate_target(&self, target: Target, size: u64) -> Result<(), FsError> {
        let request = Request {
            offset: size,
            ..Request::default()
        };
        self.call(Op::Truncate, target, request, &[], &mut []).map(|_| ())
    }

    /// A create or mkdir.
    fn make(&self, op: Op, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError> {
        let request = Request {
            mode: u64::from(mode & 0o7777),
            uid: u64::from(owner.uid),
            gid: u64::from(owner.gid),
            ..Request::default()
        };
        let meta = self.attr_call(op, Target::Path(path), request, &[])?;
        let want = if op == Op::Mkdir { FileKind::Dir } else { FileKind::File };
        if meta.kind != want {
            return Err(FsError::Io);
        }
        Ok(meta)
    }

    fn path_call(&self, op: Op, path: &str, data: &[u8]) -> Result<(), FsError> {
        self.call(op, Target::Path(path), Request::default(), data, &mut [])
            .map(|_| ())
    }
}

impl Filesystem for FuseFs {
    fn name(&self) -> &'static str {
        "fuse"
    }

    fn lookup(&self, path: &str) -> Result<Meta, FsError> {
        self.lookup_target(Target::Path(path))
    }

    fn read(&self, path: &str, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        self.read_target(Target::Path(path), offset, buf)
    }

    fn write(&self, path: &str, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        self.write_target(Target::Path(path), offset, data)
    }

    /// The node is the `(ino, generation)` the daemon reports for the file.
    fn open_node(&self, path: &str) -> Result<Option<NodeId>, FsError> {
        let (reply, _) = self.call(Op::Lookup, Target::Path(path), Request::default(), &[], &mut [])?;
        let meta = meta_of(&reply.attr)?;
        if meta.kind != FileKind::File {
            return Err(FsError::IsDir);
        }
        Ok(Some(NodeId {
            ino: meta.ino,
            generation: reply.attr.generation as u32,
        }))
    }

    fn read_node(&self, node: NodeId, offset: u64, buf: &mut [u8]) -> Result<usize, FsError> {
        self.read_target(Target::Node(node), offset, buf)
    }

    fn write_node(&self, node: NodeId, offset: u64, data: &[u8]) -> Result<usize, FsError> {
        self.write_target(Target::Node(node), offset, data)
    }

    fn stat_node(&self, node: NodeId) -> Result<Meta, FsError> {
        self.lookup_target(Target::Node(node))
    }

    fn truncate_node(&self, node: NodeId, size: u64) -> Result<(), FsError> {
        self.truncate_target(Target::Node(node), size)
    }

    fn truncate(&self, path: &str, size: u64) -> Result<(), FsError> {
        self.truncate_target(Target::Path(path), size)
    }

    fn setattr(&self, path: &str, attr: &SetAttr) -> Result<Meta, FsError> {
        let record = setattr_record(attr);
        let mut bytes = [0u8; payload::SETATTR_LEN];
        let len = record.encode(&mut bytes).ok_or(FsError::Invalid)?;
        self.attr_call(Op::SetAttr, Target::Path(path), Request::default(), &bytes[..len])
    }

    fn create(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError> {
        self.make(Op::Create, path, mode, owner)
    }

    fn mkdir(&self, path: &str, mode: u16, owner: Id) -> Result<Meta, FsError> {
        self.make(Op::Mkdir, path, mode, owner)
    }

    fn unlink(&self, path: &str) -> Result<(), FsError> {
        self.path_call(Op::Unlink, path, &[])
    }

    fn rmdir(&self, path: &str) -> Result<(), FsError> {
        self.path_call(Op::Rmdir, path, &[])
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        if to.len() > MAX_PATH {
            return Err(FsError::NameTooLong);
        }
        self.path_call(Op::Rename, from, to.as_bytes())
    }

    /// The daemon's durability point: the caller waits for its answer.
    fn flush(&self) -> Result<(), FsError> {
        self.path_call(Op::Flush, "", &[])
    }

    // `writeback` keeps the default: the periodic flusher must never wait
    // for a daemon, and durability is the daemon's own business.

    fn statfs(&self) -> Result<StatFs, FsError> {
        let mut out = [0u8; payload::STATFS_LEN];
        let (_, len) = self.call(Op::StatFs, Target::Path(""), Request::default(), &[], &mut out)?;
        let figures = StatFsRecord::decode(&out[..len]).ok_or(FsError::Io)?;
        Ok(StatFs {
            magic: figures.magic as u32,
            block_size: u32::try_from(figures.block_size).map_err(|_| FsError::Io)?,
            blocks: figures.blocks,
            blocks_free: figures.blocks_free.min(figures.blocks),
            files: figures.files,
            files_free: figures.files_free.min(figures.files),
            name_max: figures.name_max.min(fused::wire::MAX_NAME as u64) as u32,
        })
    }

    fn readdir(&self, path: &str) -> Result<Vec<DirEntry>, FsError> {
        let mut entries = Vec::new();
        let mut out = vec![0u8; MAX_DATA];
        loop {
            let request = Request {
                offset: entries.len() as u64,
                len: MAX_DATA as u64,
                ..Request::default()
            };
            let (reply, len) = self.call(Op::ReadDir, Target::Path(path), request, &[], &mut out)?;
            let count = usize::try_from(reply.count).map_err(|_| FsError::Io)?;
            if count == 0 {
                return if len == 0 { Ok(entries) } else { Err(FsError::Io) };
            }
            if entries.len() + count > MAX_DIR_ENTRIES {
                return Err(FsError::Io);
            }
            let batch = payload::decode_dirents(&out[..len], count).ok_or(FsError::Io)?;
            entries.extend(batch.into_iter().map(|entry| DirEntry {
                name: entry.name,
                ino: entry.ino,
                kind: if entry.dir { FileKind::Dir } else { FileKind::File },
            }));
        }
    }
}

/// A reply's attributes as VFS metadata, refusing what the VFS cannot hold.
fn meta_of(attr: &Attr) -> Result<Meta, FsError> {
    let kind = if attr.is_dir() {
        FileKind::Dir
    } else if attr.is_file() {
        FileKind::File
    } else {
        return Err(FsError::Io);
    };
    let id = |value: u64| u32::try_from(value).map_err(|_| FsError::Io);
    if attr.mode > 0o177777 || attr.generation > u64::from(u32::MAX) {
        return Err(FsError::Io);
    }
    Ok(Meta {
        ino: attr.ino,
        mode: attr.mode as u16,
        uid: id(attr.uid)?,
        gid: id(attr.gid)?,
        size: attr.size,
        kind,
        times: Times {
            atime: attr.atime,
            mtime: attr.mtime,
            ctime: attr.ctime,
        },
    })
}

/// The wire form of an attribute change: each selected field and its bit.
fn setattr_record(attr: &SetAttr) -> SetAttrRecord {
    let mut record = SetAttrRecord::default();
    if let Some(mode) = attr.mode {
        record.mask |= set::MODE;
        record.mode = u64::from(mode);
    }
    if let Some(uid) = attr.uid {
        record.mask |= set::UID;
        record.uid = u64::from(uid);
    }
    if let Some(gid) = attr.gid {
        record.mask |= set::GID;
        record.gid = u64::from(gid);
    }
    for (bit, value, slot) in [
        (set::ATIME, attr.atime, &mut record.atime),
        (set::MTIME, attr.mtime, &mut record.mtime),
        (set::CTIME, attr.ctime, &mut record.ctime),
    ] {
        if let Some(value) = value {
            *slot = value;
            record.mask |= bit;
        }
    }
    record
}

/// A reply status as a VFS error; an errno the VFS has no word for is an
/// I/O error.
fn error_of(code: u64) -> FsError {
    match code {
        errno::ENOENT | errno::ESTALE => FsError::NotFound,
        errno::EEXIST => FsError::Exists,
        errno::ENOTDIR => FsError::NotDir,
        errno::EISDIR => FsError::IsDir,
        errno::ENOTEMPTY => FsError::NotEmpty,
        errno::EACCES => FsError::Access,
        errno::EPERM => FsError::NotPermitted,
        errno::EROFS => FsError::ReadOnly,
        errno::EINVAL => FsError::Invalid,
        errno::ENOSPC => FsError::NoSpace,
        errno::ENAMETOOLONG => FsError::NameTooLong,
        errno::ENOSYS | errno::EOPNOTSUPP => FsError::NotSupported,
        _ => FsError::Io,
    }
}
