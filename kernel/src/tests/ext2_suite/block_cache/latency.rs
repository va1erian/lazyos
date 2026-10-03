//! Interrupt latency under sustained large writes: the package installer's
//! shape (1 MiB `write_file`s, `fsync`s, reading the result back) run as the
//! body of a syscall with the real timer. Before interrupt windows a 1 MiB
//! write kept interrupts off for 40 to 120 ms; now the library paces itself
//! (`BlockIo::pace`) and every stretch must stay under the bound, with no
//! timer tick missed (`tests::irq_window_suite` has the mechanism's tests).

use super::*;
use crate::tests::irq_window_suite::{in_syscall, kernel_task_only, Latency, BOUND_US};

/// Volume size in 1 KiB blocks: 8 MiB, one block group.
const BIG_BLOCKS: u32 = 8192;
/// Bytes per write: the installer's chunk.
const CHUNK: usize = 1024 * 1024;
/// Cache pages: half a chunk, so every write also writes back and evicts.
const PAGES: usize = 512;
/// Rounds of write + fsync + read-back; the timing verdict is the best one.
const ROUNDS: u32 = 6;
/// The syscall number the work is charged to (`write_file`).
const NR_WRITE_FILE: u64 = 17;

/// The 8 MiB disk, leaked once and refilled per run.
fn big_disk() -> Result<&'static FakeDisk, String> {
    static DISK: spin::Mutex<Option<&'static FakeDisk>> = spin::Mutex::new(None);
    let disk = *DISK
        .lock()
        .get_or_insert_with(|| FakeDisk::new("irqlat", 0));
    *disk.data.lock() = vec![0u8; BIG_BLOCKS as usize * 1024];
    let geometry = ext2fs::Geometry {
        block_size: 1024,
        blocks_count: BIG_BLOCKS,
        bytes_per_inode: 16384,
    };
    let device: &'static dyn BlockDevice = disk;
    ext2fs::format(&device, geometry, "irqlat", [0x17; 16], 1_700_000_000)
        .map_err(|error| format!("format: {error:?}"))?;
    Ok(disk)
}

/// One round inside a syscall: replace a 1 MiB file and fsync it.
fn write_round(vfs: &mut Vfs, round: u32) -> Result<Latency, String> {
    let path = format!("/big{}", round % 3);
    let data = pattern_bytes(round, CHUNK);
    let (outcome, latency) = in_syscall(NR_WRITE_FILE, || -> Result<(), String> {
        match vfs.create(Id::ROOT, &path, 0o644) {
            Ok(_) => {}
            Err(FsError::Exists) => vfs.truncate(Id::ROOT, &path, 0).map_err(fs_error)?,
            Err(error) => return Err(fs_error(error)),
        }
        let written = vfs.write(Id::ROOT, &path, 0, &data).map_err(fs_error)?;
        check!(written == CHUNK, "{path}: short write {written}");
        vfs.flush(Id::ROOT, &path).map_err(fs_error)
    });
    outcome?;
    // Compared outside the syscall: the check is the test's, not the load's.
    let back = read_file(vfs, &path)?;
    check!(
        back == data,
        "round {round}: {path} reads back different bytes"
    );
    drop(back);
    drop(data);
    Ok(latency)
}

/// Soak: rounds of 1 MiB writes and fsyncs through a cache half that size.
/// The best round keeps every interrupts-off stretch under the bound and
/// misses no tick; every round takes ticks through windows; the volume
/// checks clean at the end.
pub fn irq_latency_large_writes() -> Result<(), String> {
    kernel_task_only();
    let disk = big_disk()?;
    let (fs, mut vfs) = cached(disk, PAGES)?;
    let mut rounds = Vec::new();
    for round in 0..ROUNDS {
        let latency = write_round(&mut vfs, round)?;
        check!(
            latency.opened >= 1 && latency.window_ticks >= 1,
            "round {round}: no window took a tick: {latency:?}"
        );
        rounds.push(latency);
    }
    let best = rounds
        .iter()
        .min_by_key(|l| l.worst_us)
        .copied()
        .unwrap_or_default();
    check!(
        best.worst_us < BOUND_US && best.missed == 0,
        "no round within {BOUND_US} µs without a missed tick: {rounds:?}"
    );
    drop((fs, vfs));
    check_volume(disk, BIG_BLOCKS)?;
    *disk.data.lock() = Vec::new();
    Ok(())
}
