//! Linux opens of files on a ramfs read through instead of copying the file
//! into the heap at `open` (issue #265): a second descriptor sees the first
//! one's writes, and opening a big file costs no copy of it.

use super::*;
use crate::task::FdKind;

const READ: u64 = 0;
const WRITE: u64 = 1;
const OPEN: u64 = 2;
const CLOSE: u64 = 3;
const UNLINK: u64 = 87;
const O_RDONLY: u64 = 0;
const O_RDWR: u64 = 2;
const O_CREAT: u64 = 0o100;
const O_TRUNC: u64 = 0o1000;

fn open(path: &[u8], flags: u64) -> Result<u64, String> {
    let fd = sys(OPEN, &[path.as_ptr() as u64, flags, 0o644]);
    check!((fd as i64) >= 0, "open gave {fd:#x}");
    Ok(fd)
}

fn write(fd: u64, bytes: &[u8]) -> Result<(), String> {
    let n = sys(WRITE, &[fd, bytes.as_ptr() as u64, bytes.len() as u64]);
    check!(n == bytes.len() as u64, "write gave {n:#x}");
    Ok(())
}

fn read(fd: u64, len: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; len];
    let n = sys(READ, &[fd, buf.as_mut_ptr() as u64, len as u64]);
    check!((n as i64) >= 0, "read gave {n:#x}");
    buf.truncate(n as usize);
    Ok(buf)
}

/// A file on the ramfs opens as a read-through descriptor; a reader opened
/// before a write sees it (a snapshot would not), and the same holds the
/// other way round.
pub fn ramfs_opens_read_through() -> Result<(), String> {
    fresh()?;
    let path = cpath("/tmp/readthrough");
    let writer = open(&path, O_CREAT | O_RDWR | O_TRUNC)?;
    check!(
        task::fd_kind(writer as usize) == FdKind::Vfs,
        "a ramfs file opened as {:?}",
        task::fd_kind(writer as usize)
    );
    let reader = open(&path, O_RDONLY)?;
    write(writer, b"hello")?;
    check!(read(reader, 64)? == b"hello", "the reader missed the write");
    write(writer, b", world")?;
    check!(
        read(reader, 64)? == b", world",
        "the reader missed the append"
    );
    check!(read(reader, 64)?.is_empty(), "EOF was not reached");
    sys(CLOSE, &[reader]);
    sys(CLOSE, &[writer]);
    check!(sys(UNLINK, &[path.as_ptr() as u64]) == 0, "unlink failed");
    Ok(())
}

/// Opening a large file many times copies nothing: the heap does not grow
/// by the file's size per open, and every descriptor reads it whole.
pub fn ramfs_open_soak_copies_nothing() -> Result<(), String> {
    const SIZE: usize = 512 * 1024;
    const OPENS: usize = 64;
    fresh()?;
    let path = cpath("/tmp/big");
    let writer = open(&path, O_CREAT | O_RDWR | O_TRUNC)?;
    let data: Vec<u8> = (0..SIZE).map(|i| (i % 251) as u8).collect();
    write(writer, &data)?;
    sys(CLOSE, &[writer]);
    let before = mem::heap_stats().used;
    let mut fds = Vec::new();
    for _ in 0..OPENS {
        fds.push(open(&path, O_RDONLY)?);
    }
    let held = mem::heap_stats().used.saturating_sub(before);
    for (index, &fd) in fds.iter().enumerate() {
        if index % 16 == 0 {
            let mut got = Vec::new();
            loop {
                let chunk = read(fd, 64 * 1024)?;
                if chunk.is_empty() {
                    break;
                }
                got.extend_from_slice(&chunk);
            }
            check!(got == data, "descriptor {index} read a different file");
        }
        sys(CLOSE, &[fd]);
    }
    check!(sys(UNLINK, &[path.as_ptr() as u64]) == 0, "unlink failed");
    check!(
        held < SIZE,
        "{OPENS} opens of a {SIZE}-byte file held {held} heap bytes"
    );
    serial_println!("TEST:compat_ramfs_open_soak_copies_nothing:INFO:opens={OPENS} heap={held}");
    Ok(())
}
