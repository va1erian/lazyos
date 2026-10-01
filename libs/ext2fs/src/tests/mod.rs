//! Host tests over an in-memory [`BlockIo`](crate::BlockIo).
//!
//! Every test formats a fresh image, so none depends on another, and ends by
//! running the independent checker ([`crate::check::fsck`]) over the bytes.

use alloc::boxed::Box;
use std::string::String;
use std::vec::Vec;

use crate::check::fsck;
use crate::memio::MemIo;
use crate::{Ext2, Geometry};

mod format_tests;
mod malformed;
mod ops;
mod ops_state;
mod populate_tests;
mod seeded;
mod soak;

pub const UUID: [u8; 16] = [
    0x6c, 0x61, 0x7a, 0x79, 0x6f, 0x73, 0x2d, 0x65, 0x78, 0x74, 0x32, 0x2d, 0x74, 0x65, 0x73, 0x74,
];

/// A fixed clock, so inode times in assertions are exact.
pub fn clock() -> i64 {
    1_700_000_000
}

/// The geometry the tests use: `bytes` of `block_size` blocks, dense inodes.
pub fn geometry(bytes: u64, block_size: u32) -> Geometry {
    Geometry {
        block_size,
        blocks_count: (bytes / u64::from(block_size)) as u32,
        bytes_per_inode: 16 * 1024,
    }
}

/// A formatted, unmounted image.
pub fn formatted(bytes: u64, block_size: u32) -> MemIo {
    let io = MemIo::new(bytes as usize);
    crate::format(&io, geometry(bytes, block_size), "test", UUID, clock()).expect("format");
    io
}

pub fn open(io: &MemIo) -> Ext2 {
    Ext2::open(Box::new(io.clone()), clock).expect("open")
}

/// A formatted and mounted volume.
pub fn fresh(bytes: u64, block_size: u32) -> (MemIo, Ext2) {
    let io = formatted(bytes, block_size);
    let fs = open(&io);
    (io, fs)
}

/// Fail with every problem the checker finds in `io`'s current bytes.
pub fn assert_clean(io: &MemIo) {
    let problems: Vec<_> = fsck(&io.snapshot());
    assert!(problems.is_empty(), "fsck found problems: {problems:#?}");
}

/// The block sizes the driver supports, so each test can sweep them.
pub const BLOCK_SIZES: [u32; 3] = [1024, 2048, 4096];
