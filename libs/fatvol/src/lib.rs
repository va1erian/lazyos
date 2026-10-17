//! A FAT12/16/32 volume with long names as a user-space filesystem
//! (docs/usb-hotplug-plan.md F; issue: USB sticks that people actually own).
//!
//! `fatfuse` (the daemon) hands this crate a raw device of sectors; the
//! crate mounts it with `rust-fatfs`, caches sectors in a write-back cache
//! that is flushed at the end of every operation ([`io`]) and answers the
//! kernel's FUSE requests ([`vol::Volume`] is a [`fused::daemon::FuseFs`]).
//! Everything here is `no_std` + `alloc` and tested on the host over an
//! in-memory device ([`mem::MemDisk`]), including images made by the host's
//! own `mkfs.fat` and checked by `fsck.fat` (`tools/fat/`).
//!
//! A stick is the most hostile input the filesystem layer sees, which is why
//! it is parsed here, in a process that costs a mount when it dies, and not
//! in the kernel.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

pub mod bpb;
pub mod clock;
pub mod io;
pub mod mem;
pub mod parts;
pub mod vol;

pub use clock::Clock;
pub use io::{BlockIo, Cache, Fault, Layout, Order, MAX_IO, SECTOR};
pub use vol::{OpenError, Options, Volume};

#[cfg(test)]
mod tests;
