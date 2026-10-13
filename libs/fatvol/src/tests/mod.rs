//! Host tests: the volume over a RAM disk.

mod basic;
mod cache;
mod hostile;
mod parts;
mod soak;

use alloc::string::String;
use alloc::vec::Vec;

use fused::daemon::{Errno, FuseFs, Target};

use crate::mem::MemDisk;
use crate::{Clock, Options, Volume};

pub(crate) const LABEL: &[u8; 11] = b"LAZYTEST   ";
/// 8 MiB: a FAT12/16 geometry for every format.
pub(crate) const SECTORS: u64 = 16 * 1024;
/// 64 MiB: the smallest size `mkfs.fat -F 32` accepts without a warning.
pub(crate) const SECTORS_32: u64 = 128 * 1024;
/// 2020-02-02 02:02:02 UTC.
pub(crate) const NOW: i64 = 1_580_608_922;

pub(crate) fn opts() -> Options {
    Options {
        uid: 1000,
        gid: 1000,
        ..Options::default()
    }
}

pub(crate) fn volume(fat: u8) -> (Volume<MemDisk>, MemDisk) {
    let sectors = if fat == 32 { SECTORS_32 } else { SECTORS };
    let disk = MemDisk::formatted(sectors, fat, LABEL);
    (open(&disk, opts()), disk)
}

pub(crate) fn open(disk: &MemDisk, options: Options) -> Volume<MemDisk> {
    Volume::open(disk.clone(), options, Clock::new(NOW)).expect("volume opens")
}

pub(crate) fn path(p: &str) -> Target<'_> {
    Target::Path(p)
}

/// Write `data` to a new file, in 64 KiB pieces as the kernel would.
pub(crate) fn put(vol: &mut Volume<MemDisk>, name: &str, data: &[u8]) -> Result<(), Errno> {
    vol.create(name, 0o644, 1000, 1000)?;
    let mut at = 0;
    for piece in data.chunks(64 * 1024) {
        let wrote = vol.write(path(name), at, piece)?;
        assert_eq!(wrote, piece.len());
        at += piece.len() as u64;
    }
    Ok(())
}

pub(crate) fn get(vol: &mut Volume<MemDisk>, name: &str) -> Result<Vec<u8>, Errno> {
    let size = vol.lookup(path(name))?.size as usize;
    let mut out = alloc::vec![0u8; size];
    let mut at = 0;
    while at < size {
        let end = (at + 64 * 1024).min(size);
        let n = vol.read(path(name), at as u64, &mut out[at..end])?;
        assert!(n > 0, "short read at {at} of {size}");
        at += n;
    }
    Ok(out)
}

pub(crate) fn names(vol: &mut Volume<MemDisk>, dir: &str) -> Vec<String> {
    let mut names: Vec<String> = vol
        .readdir(dir)
        .expect("readdir")
        .into_iter()
        .map(|e| e.name)
        .collect();
    names.sort();
    names
}

pub(crate) fn pattern(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| {
            (i as u8)
                .wrapping_mul(31)
                .wrapping_add(seed)
                .wrapping_add((i >> 8) as u8)
        })
        .collect()
}

pub(crate) fn assert_errno<T: core::fmt::Debug>(got: Result<T, Errno>, want: u64) {
    assert_eq!(got.err(), Some(want), "expected errno {want}");
}

pub(crate) use fused::wire::errno as e;
