//! `getdents64` and the legacy `getdents`: read a directory as a stream of
//! Linux dirent records.
//!
//! [`super::path`] snapshots a directory into the `linux_dirent64` byte stream
//! and stores it as a plain file descriptor, so the two syscalls only differ
//! in how a record is laid out for the caller. Both hand out *whole records*:
//! a record cut off at the end of the caller's buffer would leave the stream
//! position in the middle of one, and the next call would read garbage.
//!
//! ```text
//! linux_dirent64: ino u64 | off u64 | reclen u16 | type u8 | name\0 | pad
//! linux_dirent:   ino u64 | off u64 | reclen u16 | name\0 | pad | type u8
//! ```
//!
//! Both records of one entry are the same length (the legacy name starts one
//! byte earlier, the type byte moves to the end), so one stream serves both.

use alloc::vec::Vec;

use crate::fs::vfs::{S_IFDIR, S_IFMT};
use crate::task::{self, FdKind};
use crate::user_ptr;

use super::errno::{err, EBADF, EFAULT, EINVAL, ENOTDIR};
use super::fd::fd_meta_get;

/// Offset of `d_reclen`, and of the first byte after it, in either layout.
const RECLEN_AT: usize = 16;
const TYPE_AT: usize = 18;
/// Where the name starts in the `linux_dirent64` stream.
const NAME64_AT: usize = 19;
/// Shortest record: the 19-byte header, a one-byte name and its NUL, padded
/// to the 8-byte record alignment.
const MIN_RECLEN: usize = 24;
/// Longest record: the 19-byte header and a 255-byte name (the longest a
/// filesystem stores) with its NUL, padded to the 8-byte record alignment.
const MAX_RECORD: usize = 280;

/// Which record layout the caller asked for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// `getdents64`: the stream's own layout.
    Dirent64,
    /// `getdents`: the pre-2.4 layout, with the type byte last.
    Legacy,
}

/// `getdents64(fd, buf, count)`.
pub(super) fn sys_getdents64(fd: u64, buf: u64, count: u64) -> u64 {
    read_dirents(fd, buf, count, Layout::Dirent64)
}

/// `getdents(fd, buf, count)`.
pub(super) fn sys_getdents(fd: u64, buf: u64, count: u64) -> u64 {
    read_dirents(fd, buf, count, Layout::Legacy)
}

/// Whether `fd` is a directory stream. An inherited descriptor has no
/// recorded metadata and is given the benefit of the doubt.
fn is_directory(fd: u64) -> bool {
    fd_meta_get(fd as usize).is_none_or(|meta| meta.mode & u32::from(S_IFMT) == u32::from(S_IFDIR))
}

fn read_dirents(fd: u64, buf: u64, count: u64, layout: Layout) -> u64 {
    match task::fd_kind(fd as usize) {
        FdKind::File => {}
        FdKind::Vfs => return err(ENOTDIR), // a regular file has no directory stream
        _ => return err(EBADF),
    }
    if !is_directory(fd) {
        return err(ENOTDIR);
    }
    let Some(start) = task::fd_offset(fd as usize) else {
        return err(EBADF);
    };
    let want = usize::try_from(count).unwrap_or(usize::MAX);
    // Look at least one whole record ahead, however small the caller's buffer,
    // so "the next record does not fit" is told apart from "no more entries".
    let Some(window) = task::fd_peek(fd as usize, want.max(MAX_RECORD)) else {
        return err(EBADF);
    };
    let (records, consumed) = whole_records(&window, start as u64, layout, want);
    if consumed == 0 && !window.is_empty() {
        return err(EINVAL); // the buffer cannot hold even the next record
    }
    if user_ptr::try_copy_to(buf, &records).is_err() {
        return err(EFAULT);
    }
    task::fd_advance(fd as usize, consumed);
    records.len() as u64
}

/// Re-lay out the leading whole `linux_dirent64` records of `window` (which
/// begins `start` bytes into the stream) as `layout`, as many as fit in
/// `limit` bytes. Returns the converted bytes and how many stream bytes they
/// cover (the same number: both layouts have the same record length).
///
/// `d_off` is set to the stream position of the next record, so `lseek` on
/// the directory descriptor (`seekdir`) lands on a record boundary.
fn whole_records(window: &[u8], start: u64, layout: Layout, limit: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::with_capacity(window.len().min(limit));
    let mut at = 0;
    while let Some(len) = record_len(&window[at..]).filter(|len| at + len <= limit) {
        let first = out.len();
        out.extend_from_slice(&window[at..at + len]);
        at += len;
        out[first + 8..first + 16].copy_from_slice(&(start + at as u64).to_le_bytes());
        if layout == Layout::Legacy {
            to_legacy(&mut out[first..]);
        }
    }
    (out, at)
}

/// The length of the record at the start of `rest`, if a whole, well-formed
/// one is there. The stream is built by [`super::path`], so a bad length is a
/// kernel bug; it is treated as the end rather than trusted.
fn record_len(rest: &[u8]) -> Option<usize> {
    let bytes = rest.get(RECLEN_AT..RECLEN_AT + 2)?;
    let len = usize::from(u16::from_le_bytes([bytes[0], bytes[1]]));
    let valid = len >= MIN_RECLEN && len % 8 == 0 && len <= rest.len();
    valid.then_some(len)
}

/// Turn one `linux_dirent64` record into a `linux_dirent` one in place: the
/// name moves down a byte and the type byte becomes the record's last.
fn to_legacy(record: &mut [u8]) {
    let len = record.len();
    let kind = record[TYPE_AT];
    let name_len = record[NAME64_AT..]
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(len - NAME64_AT - 1);
    // The name and its NUL slide down one byte; the vacated tail (padding and
    // the old NUL position) is cleared before the type byte goes in last.
    record.copy_within(NAME64_AT..NAME64_AT + name_len + 1, TYPE_AT);
    record[TYPE_AT + name_len + 1..].fill(0);
    record[len - 1] = kind;
}
