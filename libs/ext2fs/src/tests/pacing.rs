//! [`BlockIo::pace`]: the host's pause between units of work. The kernel
//! runs the library with interrupts off and opens an interrupt window there,
//! so every long operation must pace itself at least once per block it
//! touches, cached or not; and the byte-skipping bitmap scan behind block
//! allocation must find exactly what a bit-by-bit scan finds.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::*;
use crate::{BlockIo, IoError};

/// A [`MemIo`] that counts the library's pace calls.
struct Paced {
    io: MemIo,
    paces: Arc<AtomicU64>,
}

impl BlockIo for Paced {
    fn sector_count(&self) -> u64 {
        self.io.sector_count()
    }
    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), IoError> {
        self.io.read_sectors(lba, buf)
    }
    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), IoError> {
        self.io.write_sectors(lba, buf)
    }
    fn flush(&self) -> Result<(), IoError> {
        self.io.flush()
    }
    fn is_writable(&self) -> bool {
        self.io.is_writable()
    }
    fn pace(&self) {
        self.paces.fetch_add(1, Ordering::Relaxed);
    }
}

fn paced(io: &MemIo, cache_blocks: Option<usize>) -> (Ext2, Arc<AtomicU64>) {
    let paces = Arc::new(AtomicU64::new(0));
    let device = Box::new(Paced {
        io: io.clone(),
        paces: paces.clone(),
    });
    let fs = match cache_blocks {
        Some(blocks) => Ext2::open_cached(device, clock, crate::CacheConfig::heap(blocks)),
        None => Ext2::open(device, clock),
    }
    .expect("open");
    (fs, paces)
}

fn taken(paces: &AtomicU64) -> u64 {
    paces.swap(0, Ordering::Relaxed)
}

/// Writing, reading back, truncating and committing a 256 KiB file paces at
/// least once per data block each, on every block size, direct and through
/// a cache smaller than the file.
#[test]
fn long_operations_pace_once_per_block() {
    for block_size in BLOCK_SIZES {
        for cache in [None, Some(16)] {
            let io = formatted(4 << 20, block_size);
            let (fs, paces) = paced(&io, cache);
            let data: Vec<u8> = (0..256 * 1024u32).map(|n| (n * 7 / 3) as u8).collect();
            let blocks = (data.len() / block_size as usize) as u64;
            fs.create("/f", 0o644, crate::Owner::ROOT).expect("create");
            taken(&paces);

            assert_eq!(fs.write("/f", 0, &data).expect("write"), data.len());
            let write = taken(&paces);
            let mut back = std::vec![0u8; data.len()];
            assert_eq!(fs.read("/f", 0, &mut back).expect("read"), data.len());
            let read = taken(&paces);
            assert_eq!(back, data);
            fs.truncate("/f", 0).expect("truncate");
            fs.flush().expect("flush");
            let shrink = taken(&paces);

            let what = std::format!("{block_size}-byte blocks, cache {cache:?}");
            assert!(write >= blocks, "{what}: write paced {write} for {blocks}");
            assert!(read >= blocks, "{what}: read paced {read} for {blocks}");
            assert!(shrink >= blocks, "{what}: truncate paced {shrink}");
            drop(fs);
            assert_clean(&io);
        }
    }
}

/// Scanning a large directory paces per directory block.
#[test]
fn directory_scans_pace_per_block() {
    let io = formatted(8 << 20, 1024);
    let (fs, paces) = paced(&io, Some(64));
    for n in 0..300 {
        fs.create(
            &std::format!("/name-long-enough-to-fill-blocks-{n:04}"),
            0o644,
            crate::Owner::ROOT,
        )
        .expect("create");
    }
    taken(&paces);
    let entries = fs.readdir("/").expect("readdir");
    assert_eq!(entries.len(), 301); // and lost+found
    let scan = taken(&paces);
    // 300 entries of ~48 bytes fill at least 14 one-KiB blocks.
    assert!(scan >= 14, "readdir paced {scan} times");
    drop(fs);
    assert_clean(&io);
}

/// The bit-by-bit reference scan.
fn naive(buf: &[u8], start: u32, bits: u32) -> Option<u32> {
    (start..bits).find(|&index| buf[(index / 8) as usize] & (1 << (index % 8)) == 0)
}

/// The byte-skipping scan agrees with the reference on bitmaps of runs of
/// full bytes, scattered holes and a hole in the last partial byte, for
/// every start and bit count up to the buffer.
#[test]
fn bitmap_scan_matches_bit_by_bit() {
    let mut seed = 0x9e37_79b9u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for _ in 0..300 {
        let len = 1 + (next() % 64) as usize;
        let mut buf: Vec<u8> = (0..len)
            .map(|_| match next() % 4 {
                0 => next() as u8,
                _ => 0xFF,
            })
            .collect();
        if next() % 2 == 0 {
            buf.iter_mut().for_each(|byte| *byte = 0xFF);
            let last = len * 8 - 1 - (next() % 8) as usize;
            buf[last / 8] &= !(1 << (last % 8));
        }
        let total = (len * 8) as u32;
        for _ in 0..20 {
            let bits = next() % (total + 1);
            let start = next() % (bits + 1);
            let got = Ext2::bitmap_find_zero(&buf, start, bits).expect("in bounds");
            assert_eq!(
                got,
                naive(&buf, start, bits),
                "{buf:x?} start {start} bits {bits}"
            );
        }
    }
}
