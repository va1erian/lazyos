//! Damaged and hostile images: the daemon must reject, fail or shrug, never
//! panic and never hang (a panic costs the mount; a hang costs it until the
//! daemon is killed).

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::string::String;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fused::daemon::FuseFs;

use super::*;
use crate::mem::MemDisk;
use crate::OpenError;

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    Rejected,
    Used,
    Panicked(String),
}

/// Walk a volume and change it a little; reads and writes may fail with
/// errno, which is fine.
fn exercise(vol: &mut Volume<MemDisk>, dir: &str, depth: u32) {
    let Ok(entries) = vol.readdir(dir) else {
        return;
    };
    for entry in entries.into_iter().take(64) {
        let path_of = if dir.is_empty() {
            entry.name.clone()
        } else {
            alloc::format!("{dir}/{}", entry.name)
        };
        if entry.dir {
            if depth < 3 {
                exercise(vol, &path_of, depth + 1);
            }
            continue;
        }
        let mut buf = [0u8; 4096];
        let mut at = 0u64;
        for _ in 0..16 {
            match vol.read(path(&path_of), at, &mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => at += n as u64,
            }
        }
    }
    let _ = vol.create("zz-new.txt", 0o644, 0, 0);
    let _ = vol.write(path("zz-new.txt"), 0, &[7u8; 5000]);
    let _ = vol.statfs();
    let _ = vol.unlink("zz-new.txt");
    let _ = vol.settle();
}

fn run(image: Vec<u8>) -> Outcome {
    let attempt = catch_unwind(AssertUnwindSafe(|| {
        let disk = MemDisk::from_image(&image);
        match Volume::open(disk, opts(), Clock::new(NOW)) {
            Err(OpenError::NotFat | OpenError::Io(_)) => Outcome::Rejected,
            Ok(mut vol) => {
                exercise(&mut vol, "", 0);
                Outcome::Used
            }
        }
    }));
    attempt.unwrap_or_else(|payload| {
        let text = payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&str>().map(|s| String::from(*s)))
            .unwrap_or_default();
        Outcome::Panicked(text)
    })
}

/// [`run`] on its own thread with a deadline.
fn run_bounded(image: Vec<u8>, label: &str) -> Outcome {
    let (tx, rx) = mpsc::channel();
    thread::Builder::new()
        .stack_size(8 << 20)
        .spawn(move || {
            let _ = tx.send(run(image));
        })
        .unwrap();
    rx.recv_timeout(Duration::from_secs(20))
        .unwrap_or_else(|_| panic!("{label}: hung"))
}

fn populated(fat: u8) -> Vec<u8> {
    let (mut vol, disk) = volume(fat);
    vol.mkdir("dir", 0o755, 0, 0).unwrap();
    put(&mut vol, "dir/inner.txt", &pattern(30_000, 1)).unwrap();
    put(&mut vol, "top.bin", &pattern(70_000, 2)).unwrap();
    put(&mut vol, "a long name with spaces.txt", b"hello").unwrap();
    vol.settle().unwrap();
    disk.image()
}

/// A tiny deterministic generator.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn garbage_is_not_a_volume() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for len in [0usize, 512, 4096, 1 << 20] {
        let image: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        assert_eq!(run_bounded(image, "random"), Outcome::Rejected, "len {len}");
    }
    assert_eq!(
        run_bounded(alloc::vec![0; 1 << 20], "zeros"),
        Outcome::Rejected
    );
    assert_eq!(
        run_bounded(alloc::vec![0xFF; 1 << 20], "ones"),
        Outcome::Rejected
    );
}

#[test]
fn damaged_boot_sectors_never_panic_or_hang() {
    let mut panics = Vec::new();
    for fat in [16, 32] {
        let base = populated(fat);
        for offset in 0..96usize {
            for value in [0x00u8, 0x01, 0x7F, 0x80, 0xFF] {
                let mut image = base.clone();
                if image[offset] == value {
                    continue;
                }
                image[offset] = value;
                let label = alloc::format!("FAT{fat} boot[{offset}]={value:#x}");
                if let Outcome::Panicked(text) = run_bounded(image, &label) {
                    panics.push(alloc::format!("{label}: {text}"));
                }
            }
        }
    }
    assert!(
        panics.is_empty(),
        "{} panics:\n{}",
        panics.len(),
        panics.join("\n")
    );
}

#[test]
fn damaged_allocation_tables_never_panic_or_hang() {
    let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
    let mut panics = Vec::new();
    for fat in [16, 32] {
        let base = populated(fat);
        // The FAT starts at the reserved sectors (32 for FAT32, 1 for FAT16).
        let fat_start = if fat == 32 { 32 * 512 } else { 512 };
        for round in 0..40 {
            let mut image = base.clone();
            for _ in 0..(1 + round % 8) {
                let at = fat_start + (rng.next() as usize % 2048) * 4;
                let value = rng.next() as u32;
                image[at..at + 4].copy_from_slice(&value.to_le_bytes());
            }
            let label = alloc::format!("FAT{fat} table round {round}");
            if let Outcome::Panicked(text) = run_bounded(image, &label) {
                panics.push(alloc::format!("{label}: {text}"));
            }
        }
    }
    assert!(
        panics.is_empty(),
        "{} panics:\n{}",
        panics.len(),
        panics.join("\n")
    );
}

#[test]
fn a_directory_that_loops_ends() {
    // Point a directory's only cluster at itself: its listing must stop.
    for fat in [16, 32] {
        let (mut vol, disk) = volume(fat);
        vol.mkdir("loop", 0o755, 0, 0).unwrap();
        for i in 0..40 {
            put(&mut vol, &alloc::format!("loop/f{i}"), b"x").unwrap();
        }
        vol.settle().unwrap();
        let mut image = disk.image();
        // Find "loop"'s cluster from its entry, then make its FAT entry
        // refer to itself.
        let attr_ino = vol.lookup(path("loop")).unwrap().ino;
        assert!(attr_ino > 1);
        let fat_start = if fat == 32 { 32 * 512 } else { 512 };
        // Cluster 2 is the first allocated: the root on FAT32, "loop" on
        // FAT16 (whose root is outside the data area).
        let cluster = if fat == 32 { 3 } else { 2 };
        let entry = fat_start + cluster * if fat == 32 { 4 } else { 2 };
        if fat == 32 {
            image[entry..entry + 4].copy_from_slice(&(cluster as u32).to_le_bytes());
        } else {
            image[entry..entry + 2].copy_from_slice(&(cluster as u16).to_le_bytes());
        }
        let outcome = run_bounded(image, "looping directory");
        assert!(
            matches!(outcome, Outcome::Used | Outcome::Rejected),
            "{outcome:?}"
        );
    }
}

#[test]
fn damaged_directory_entries_never_panic_or_hang() {
    let mut rng = Rng(0x0123_4567_89AB_CDEF);
    let mut panics = Vec::new();
    for fat in [16, 32] {
        let base = populated(fat);
        // The short entries and the long-name entries before them.
        let needles: [&[u8]; 3] = [b"TOP     BIN", b"DIR        ", b"INNER   TXT"];
        let mut spots = Vec::new();
        for needle in needles {
            if let Some(at) = base.windows(needle.len()).position(|w| w == needle) {
                spots.push(at);
            }
        }
        assert!(!spots.is_empty(), "no directory entry found to damage");
        for round in 0..60 {
            let mut image = base.clone();
            let spot = spots[round % spots.len()];
            for _ in 0..(1 + round % 6) {
                // Anywhere in the entry and the long-name entries before it:
                // names, attributes, first cluster, sizes, sequence numbers.
                let at = spot - (rng.next() as usize % 96).min(spot) + (rng.next() as usize % 32);
                image[at] = rng.next() as u8;
            }
            let label = alloc::format!("FAT{fat} directory round {round}");
            if let Outcome::Panicked(text) = run_bounded(image, &label) {
                panics.push(alloc::format!("{label}: {text}"));
            }
        }
    }
    assert!(
        panics.is_empty(),
        "{} panics:\n{}",
        panics.len(),
        panics.join("\n")
    );
}
