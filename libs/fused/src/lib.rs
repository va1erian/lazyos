//! User-space filesystems (docs/smb-plan.md §3, stage F1).
//!
//! A ring-3 daemon serves a directory tree to the kernel through syscall 35:
//! it mounts at `/mnt/<name>`, takes one request at a time (`NEXT`), and
//! answers it (`REPLY`). The kernel's `fs/fuse` backend turns every VFS call
//! on that mount into such a request and parks the caller until the answer
//! comes. This crate is the one definition of that protocol, linked by both
//! sides:
//!
//! * [`wire`]: the syscall's operation codes and the fixed request and reply
//!   records, word for word.
//! * [`payload`]: what travels in the data buffer beside a record (paths,
//!   directory entries, `statfs` figures, attribute changes), encoded and
//!   decoded with every length checked.
//! * [`daemon`]: the [`daemon::FuseFs`] trait a filesystem implements and
//!   [`daemon::serve_one`], which takes one request from a
//!   [`daemon::Provider`] and answers it. The kernel test suite drives it
//!   against the real kernel path; host tests drive it against a fake.
//! * [`memfs`]: an in-memory filesystem, the `memfuse` daemon's tree.
//! * [`inodes`]: inode numbers by path, for daemons over a network share.
//!
//! Nothing a daemon sends is trusted by the kernel and nothing the kernel
//! sends is trusted by a daemon: both sides decode through this crate, which
//! never panics on hostile input.

#![no_std]

extern crate alloc;

pub mod daemon;
pub mod inodes;
pub mod memfs;
pub mod payload;
pub mod wire;

#[cfg(test)]
mod tests;
