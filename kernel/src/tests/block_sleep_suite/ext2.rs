//! A cached ext2 volume on real virtio-blk, driven by several kernel threads
//! at once through the kernel adapter: every call holds the volume's gate,
//! so every request may sleep, and the threads meet each other at the gate
//! (a yielding lock) while one of them is parked inside the library.

use alloc::boxed::Box;
use alloc::sync::Arc;

use super::*;
use crate::block::partition::Partition;
use crate::block::SECTOR_SIZE;
use crate::fs::ext2::Ext2;
use crate::fs::vfs::{Filesystem, FsError, Id};

const THREADS: usize = 4;
const ROUNDS: usize = 40;
const LIMIT_NS: u64 = 300_000_000_000;
/// The scratch disk's lower half (the bcache suite uses the upper one).
const START: u64 = 64;
const SECTORS: u64 = 16 * 1024 - 128;

static VOLUME: spin::Mutex<Option<Arc<Ext2>>> = spin::Mutex::new(None);
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn fs_text(error: FsError) -> String {
    format!("{error:?}")
}

fn content(len: usize, salt: u64) -> Vec<u8> {
    (0..len as u64).map(|at| pattern(at, salt)).collect()
}

/// One thread's work on its own directory: write, read back by path and by
/// node, rewrite, delete; thread 0 also commits now and then.
fn work(fs: &Ext2, me: usize) -> Result<(), String> {
    let mut rng = Rng::seeded();
    let dir = format!("t{me}");
    fs.mkdir(&dir, 0o755, Id::ROOT).map_err(fs_text)?;
    for round in 0..ROUNDS {
        let path = format!("{dir}/f{}", round % 6);
        match fs.create(&path, 0o644, Id::ROOT) {
            Ok(_) => {}
            Err(FsError::Exists) => fs.truncate(&path, 0).map_err(fs_text)?,
            Err(error) => return Err(format!("create {path}: {error:?}")),
        }
        let len = 1 + rng.below(240 * 1024) as usize;
        let salt = rng.next();
        let data = content(len, salt);
        let written = fs.write(&path, 0, &data).map_err(fs_text)?;
        check!(written == len, "{path}: wrote {written} of {len}");
        let mut back = vec![0u8; len];
        let read = fs.read(&path, 0, &mut back).map_err(fs_text)?;
        check!(
            read == len && back == data,
            "{path}: read back by path differs"
        );
        let node = fs.open_node(&path).map_err(fs_text)?.ok_or("no node")?;
        back.fill(0);
        let read = fs.read_node(node, 0, &mut back).map_err(fs_text)?;
        check!(
            read == len && back == data,
            "{path}: read back by node differs"
        );
        if round % 3 == 2 {
            fs.unlink(&path).map_err(fs_text)?;
            check!(
                fs.read_node(node, 0, &mut back[..1]) == Err(FsError::NotFound),
                "{path}: a node outlived its unlink"
            );
        }
        if me == 0 && round % 5 == 4 {
            fs.flush().map_err(fs_text)?;
        }
    }
    for index in 0..6 {
        match fs.unlink(&format!("{dir}/f{index}")) {
            Ok(()) | Err(FsError::NotFound) => {}
            Err(error) => return Err(format!("cleanup: {error:?}")),
        }
    }
    fs.rmdir(&dir).map_err(fs_text)
}

extern "C" fn worker() -> ! {
    let me = NEXT.fetch_add(1, Ordering::Relaxed);
    let volume = VOLUME.lock().clone();
    let result = match volume {
        Some(fs) => work(&fs, me),
        None => Err(String::from("no volume")),
    };
    finish_thread(result)
}

/// Four threads create, write, read back (by path and by node), rewrite and
/// delete files on one cached volume, one of them committing as it goes,
/// while their requests sleep: every byte reads back, nothing leaks (the
/// free counts return to the formatted volume's), and the synced volume
/// remounts clean with the same counts.
pub fn soak_threads_on_one_volume() -> Result<(), String> {
    let Some(disk) = scratch("block_sleep_soak_ext2_threads") else {
        return Ok(());
    };
    let device: &'static dyn BlockDevice = Box::leak(Box::new(Partition::new(
        disk,
        "sleep-scratch",
        START,
        SECTORS,
    )));
    let geometry = ext2fs::Geometry::for_size(SECTORS * SECTOR_SIZE as u64);
    ext2fs::format(&device, geometry, "sleep-soak", [0x5C; 16], 1_700_000_000)
        .map_err(|error| format!("format: {error:?}"))?;
    let fs = Arc::new(Ext2::open_cached(device).map_err(fs_text)?);
    let baseline = (
        fs.free_blocks().map_err(fs_text)?,
        fs.free_inodes().map_err(fs_text)?,
    );
    *VOLUME.lock() = Some(fs.clone());
    NEXT.store(0, Ordering::Relaxed);
    let parks = crate::block::iowait::test_hooks::PARKS.load(Ordering::Relaxed);
    let outcome = run_threads(THREADS, worker, LIMIT_NS, || Ok(()));
    *VOLUME.lock() = None;
    outcome?;
    let slept = crate::block::iowait::test_hooks::PARKS.load(Ordering::Relaxed) - parks;
    serial_println!("TEST:block_sleep_soak_ext2_threads:INFO:{slept} parks");
    fs.flush().map_err(fs_text)?;
    let after = (
        fs.free_blocks().map_err(fs_text)?,
        fs.free_inodes().map_err(fs_text)?,
    );
    check!(
        after == baseline,
        "free (blocks, inodes) {after:?}, formatted {baseline:?}"
    );
    drop(fs);
    let remounted = Ext2::open(device).map_err(fs_text)?;
    check!(
        remounted.was_clean_at_mount(),
        "the synced volume is not clean"
    );
    let again = (
        remounted.free_blocks().map_err(fs_text)?,
        remounted.free_inodes().map_err(fs_text)?,
    );
    check!(
        again == baseline,
        "after a remount: {again:?}, formatted {baseline:?}"
    );
    check!(slept > 0, "no request slept");
    Ok(())
}
