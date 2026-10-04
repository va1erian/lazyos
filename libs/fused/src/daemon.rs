//! The daemon side: a [`FuseFs`] is a directory tree, a [`Provider`] is where
//! requests come from (syscall 35 in a real daemon, the kernel's own entry
//! points in the kernel suite, a script in host tests), and [`serve_one`]
//! takes one request and answers it.
//!
//! Every request is decoded here before the filesystem sees it: an unknown
//! op, a payload whose lengths do not add up, or a path that is not a clean
//! relative path is answered with `EINVAL` and never reaches the tree.

use alloc::vec;
use alloc::vec::Vec;

use crate::payload::{self, DirEnt, SetAttrRecord, StatFsRecord};
use crate::wire::{errno, Attr, Op, Reply, Request, MAX_DATA, MAX_PAYLOAD};

/// What a request names: a path relative to the mount root, or a node a
/// lookup returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target<'a> {
    Path(&'a str),
    Node { ino: u64, generation: u64 },
}

/// A failed operation: a Linux errno ([`errno`]).
pub type Errno = u64;

/// A directory tree served to the kernel. Paths are already validated
/// ([`payload::parse_path`]); permissions were checked by the kernel's VFS
/// against the attributes this tree reports.
pub trait FuseFs {
    fn lookup(&mut self, target: Target) -> Result<Attr, Errno>;
    /// Up to `out.len()` bytes at `offset`; 0 at the end of the file.
    fn read(&mut self, target: Target, offset: u64, out: &mut [u8]) -> Result<usize, Errno>;
    /// Write `data` at `offset`, extending the file; returns the count.
    fn write(&mut self, target: Target, offset: u64, data: &[u8]) -> Result<usize, Errno>;
    fn truncate(&mut self, target: Target, size: u64) -> Result<(), Errno>;
    fn setattr(&mut self, _path: &str, _change: &SetAttrRecord) -> Result<Attr, Errno> {
        Err(errno::EOPNOTSUPP)
    }
    fn create(&mut self, path: &str, mode: u16, uid: u32, gid: u32) -> Result<Attr, Errno>;
    fn mkdir(&mut self, path: &str, mode: u16, uid: u32, gid: u32) -> Result<Attr, Errno>;
    fn unlink(&mut self, path: &str) -> Result<(), Errno>;
    fn rmdir(&mut self, path: &str) -> Result<(), Errno>;
    fn rename(&mut self, from: &str, to: &str) -> Result<(), Errno>;
    /// Every entry of the directory (without `.` and `..`), in a stable order.
    fn readdir(&mut self, path: &str) -> Result<Vec<DirEnt>, Errno>;
    /// Make everything written so far durable before answering.
    fn flush(&mut self) -> Result<(), Errno> {
        Ok(())
    }
    fn statfs(&mut self) -> Result<StatFsRecord, Errno> {
        Err(errno::ENOSYS)
    }
}

/// Where requests come from and replies go.
pub trait Provider {
    /// Wait (bounded) for the next request; its payload lands at the start
    /// of `payload` (at least [`MAX_PAYLOAD`] bytes). `Ok(None)`: none came.
    fn next(&mut self, payload: &mut [u8]) -> Result<Option<Request>, i64>;
    /// Answer a request; `data` is the reply's payload (`data_len` bytes).
    fn reply(&mut self, reply: &Reply, data: &[u8]) -> Result<(), i64>;
}

/// The buffers one serve loop reuses.
pub struct Buffers {
    payload: Vec<u8>,
    out: Vec<u8>,
}

impl Buffers {
    pub fn new() -> Buffers {
        Buffers {
            payload: vec![0; MAX_PAYLOAD],
            out: vec![0; MAX_DATA],
        }
    }
}

impl Default for Buffers {
    fn default() -> Self {
        Buffers::new()
    }
}

/// Take one request from `provider` and answer it from `fs`. `Ok(false)`
/// when none came before the provider's wait ended; an error is the
/// provider's own (the mount is gone, say).
pub fn serve_one(
    fs: &mut dyn FuseFs,
    provider: &mut dyn Provider,
    buffers: &mut Buffers,
) -> Result<bool, i64> {
    let Some(request) = provider.next(&mut buffers.payload)? else {
        return Ok(false);
    };
    let (mut reply, data_len) = match handle(fs, &request, &buffers.payload, &mut buffers.out) {
        Ok(answer) => answer,
        Err(code) => (
            Reply {
                status: if code == 0 { errno::EIO } else { code },
                ..Reply::default()
            },
            0,
        ),
    };
    reply.tag = request.tag;
    reply.data_len = data_len as u64;
    provider.reply(&reply, &buffers.out[..data_len])?;
    Ok(true)
}

/// Answer one decoded request: the reply (without its tag) and how many
/// bytes of `out` it carries.
fn handle(
    fs: &mut dyn FuseFs,
    request: &Request,
    payload: &[u8],
    out: &mut [u8],
) -> Result<(Reply, usize), Errno> {
    let op = request.operation().ok_or(errno::ENOSYS)?;
    if request.by_node() && !op.takes_node() {
        return Err(errno::EINVAL);
    }
    let total = request.payload_len().ok_or(errno::EINVAL)?;
    let payload = payload.get(..total).ok_or(errno::EINVAL)?;
    let (path_bytes, data) = payload.split_at(request.path_len as usize);
    let path = || payload::parse_path(path_bytes).ok_or(errno::EINVAL);
    let target = || -> Result<Target, Errno> {
        if request.by_node() {
            Ok(Target::Node {
                ino: request.ino,
                generation: request.generation,
            })
        } else {
            path().map(Target::Path)
        }
    };
    let with_attr = |attr: Attr| (Reply { attr, ..Reply::default() }, 0);
    let counted = |count: usize| {
        (
            Reply {
                count: count as u64,
                ..Reply::default()
            },
            0,
        )
    };
    let mode = (request.mode & 0o7777) as u16;
    let (uid, gid) = (request.uid as u32, request.gid as u32);
    Ok(match op {
        Op::Lookup => with_attr(fs.lookup(target()?)?),
        Op::Read => {
            let room = usize::try_from(request.len).map_or(MAX_DATA, |n| n.min(MAX_DATA));
            let room = room.min(out.len());
            let read = fs.read(target()?, request.offset, &mut out[..room])?;
            if read > room {
                return Err(errno::EIO);
            }
            (
                Reply {
                    count: read as u64,
                    ..Reply::default()
                },
                read,
            )
        }
        Op::Write => {
            let written = fs.write(target()?, request.offset, data)?;
            counted(written.min(data.len()))
        }
        Op::Truncate => {
            fs.truncate(target()?, request.offset)?;
            counted(0)
        }
        Op::SetAttr => {
            let change = SetAttrRecord::decode(data).ok_or(errno::EINVAL)?;
            with_attr(fs.setattr(path()?, &change)?)
        }
        Op::Create => with_attr(fs.create(path()?, mode, uid, gid)?),
        Op::Mkdir => with_attr(fs.mkdir(path()?, mode, uid, gid)?),
        Op::Unlink => {
            fs.unlink(path()?)?;
            counted(0)
        }
        Op::Rmdir => {
            fs.rmdir(path()?)?;
            counted(0)
        }
        Op::Rename => {
            let to = payload::parse_path(data).ok_or(errno::EINVAL)?;
            fs.rename(path()?, to)?;
            counted(0)
        }
        Op::ReadDir => readdir(fs, path()?, request, out)?,
        Op::Flush => {
            fs.flush()?;
            counted(0)
        }
        Op::StatFs => {
            let figures = fs.statfs()?;
            let len = figures.encode(out).ok_or(errno::EIO)?;
            (Reply::default(), len)
        }
    })
}

/// Entries from index `request.offset` on, as many as fit in its room.
fn readdir(
    fs: &mut dyn FuseFs,
    path: &str,
    request: &Request,
    out: &mut [u8],
) -> Result<(Reply, usize), Errno> {
    let entries = fs.readdir(path)?;
    let room = usize::try_from(request.len).map_or(out.len(), |n| n.min(out.len()));
    let start = usize::try_from(request.offset).unwrap_or(usize::MAX);
    let (mut at, mut count) = (0, 0u64);
    // A name the wire cannot carry is left out, before indexing, so the
    // kernel's next index still lands on the entry after the last one sent.
    let valid = entries.iter().filter(|entry| payload::valid_name(&entry.name));
    for entry in valid.skip(start) {
        match payload::encode_dirent(&mut out[..room], at, entry.ino, entry.dir, &entry.name) {
            Some(next) => at = next,
            None => break,
        }
        count += 1;
    }
    Ok((
        Reply {
            count,
            ..Reply::default()
        },
        at,
    ))
}
