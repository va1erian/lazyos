//! Kernel threads parked in virtio-blk while the device reads and writes
//! their own buffers, next to a caller that spins on the same queue.

use super::*;
use crate::block::iowait::test_hooks;
use crate::block::{Wait, SECTOR_SIZE};

/// Writers, each with its own window of the scratch disk.
const THREADS: usize = 4;
/// Sectors per thread window (2 MiB), past the kernel task's window.
const WINDOW: u64 = 4096;
const FIRST_WINDOW: u64 = 4096;
const ROUNDS: usize = 30;
const LIMIT_NS: u64 = 120_000_000_000;

/// Which window the next thread takes.
static NEXT_WINDOW: AtomicUsize = AtomicUsize::new(0);

/// `len` bytes of pattern `salt` for disk offset `at`, inside a buffer that
/// starts `skew` bytes into its allocation (so pieces straddle pages).
fn staged(at: u64, len: usize, salt: u64, skew: usize) -> Vec<u8> {
    let mut buf = vec![0u8; skew + len];
    for (index, byte) in buf[skew..].iter_mut().enumerate() {
        *byte = pattern(at + index as u64, salt);
    }
    buf
}

/// Write a random extent of `window` in several unaligned segments, read it
/// back in another segmentation, compare.
fn round_trip(
    disk: &dyn BlockDevice,
    window: u64,
    rng: &mut Rng,
    round: usize,
) -> Result<(), String> {
    let sectors = 1 + rng.below(600);
    let lba = window + rng.below(WINDOW - sectors);
    let len = sectors as usize * SECTOR_SIZE;
    let salt = rng.next();
    let at = lba * SECTOR_SIZE as u64;
    let skew = rng.below(4096) as usize;
    let source = staged(at, len, salt, skew);
    let data = &source[skew..];
    let cut = (rng.below(sectors) as usize * SECTOR_SIZE).min(len);
    let (head, tail) = data.split_at(cut);
    disk.write_sectors_vectored_with(lba, &[head, tail], Wait::MaySleep)
        .map_err(|e| format!("round {round}: write {lba}+{sectors}: {e:?}"))?;
    let mut back = vec![0u8; len + 7];
    {
        let body = &mut back[7..];
        let split = (rng.below(len as u64) as usize).min(len);
        let (first, second) = body.split_at_mut(split);
        disk.read_sectors_vectored_with(lba, &mut [first, second], Wait::MaySleep)
            .map_err(|e| format!("round {round}: read {lba}+{sectors}: {e:?}"))?;
    }
    check!(
        &back[7..] == data,
        "round {round}: {lba}+{sectors} read back differently"
    );
    Ok(())
}

extern "C" fn writer() -> ! {
    let window = FIRST_WINDOW + NEXT_WINDOW.fetch_add(1, Ordering::Relaxed) as u64 * WINDOW;
    let mut rng = Rng::seeded();
    let result = match super::scratch("block_sleep_virtio_threads") {
        Some(disk) => (0..ROUNDS).try_for_each(|round| round_trip(disk, window, &mut rng, round)),
        None => Ok(()),
    };
    finish_thread(result)
}

/// Four threads write and read back their windows with requests that sleep,
/// while the kernel task spins its own requests through the same queue
/// between its naps: every byte comes back, and the threads did sleep.
pub fn threads_and_a_spinner() -> Result<(), String> {
    let Some(disk) = scratch("block_sleep_virtio_threads") else {
        return Ok(());
    };
    NEXT_WINDOW.store(0, Ordering::Relaxed);
    let parks = test_hooks::PARKS.load(Ordering::Relaxed);
    let mut rng = Rng::seeded();
    let mut spins = 0usize;
    run_threads(THREADS, writer, LIMIT_NS, || {
        spins += 1;
        // The kernel task's window is the first one; it never sleeps.
        let sectors = 1 + rng.below(64);
        let lba = 64 + rng.below(FIRST_WINDOW - 64 - sectors);
        let salt = rng.next();
        let data = staged(
            lba * SECTOR_SIZE as u64,
            sectors as usize * SECTOR_SIZE,
            salt,
            0,
        );
        disk.write_sectors(lba, &data)
            .map_err(|e| format!("spinning write: {e:?}"))?;
        let mut back = vec![0u8; data.len()];
        disk.read_sectors(lba, &mut back)
            .map_err(|e| format!("spinning read: {e:?}"))?;
        check!(back == data, "the spinning caller read back differently");
        Ok(())
    })?;
    let slept = test_hooks::PARKS.load(Ordering::Relaxed) - parks;
    serial_println!(
        "TEST:block_sleep_virtio_threads:INFO:{} round trips, {slept} parks, {spins} spinning round trips",
        THREADS * ROUNDS
    );
    check!(slept > 0, "no request ever slept");
    check!(spins > 0, "the spinning caller never ran");
    Ok(())
}

extern "C" fn killed_reader() -> ! {
    test_hooks::KILLED.store(task::current(), Ordering::Relaxed);
    let mut rng = Rng::seeded();
    let result = match super::scratch("block_sleep_virtio_killed_waiter") {
        Some(disk) => (0..4).try_for_each(|round| {
            // Large transfers: several requests the device works on while
            // the "killed" waiter naps.
            let lba = FIRST_WINDOW;
            let len = 1 << 20;
            let salt = rng.next();
            let data = staged(lba * SECTOR_SIZE as u64, len, salt, 0);
            disk.write_sectors_vectored_with(lba, &[&data], Wait::MaySleep)
                .map_err(|e| format!("round {round}: write: {e:?}"))?;
            let mut back = vec![0u8; len];
            disk.read_sectors_vectored_with(lba, &mut [&mut back], Wait::MaySleep)
                .map_err(|e| format!("round {round}: read: {e:?}"))?;
            check!(back == data, "round {round}: read back differently");
            Ok(())
        }),
        None => Ok(()),
    };
    test_hooks::KILLED.store(usize::MAX, Ordering::Relaxed);
    finish_thread(result)
}

/// A waiter whose waits return at once (a task being killed, #558) never
/// leaves its request: it naps until the device is done with its buffers,
/// and its transfers complete with the right bytes.
pub fn killed_waiter_finishes() -> Result<(), String> {
    if scratch("block_sleep_virtio_killed_waiter").is_none() {
        return Ok(());
    }
    let naps = test_hooks::KILLED_NAPS.load(Ordering::Relaxed);
    run_threads(1, killed_reader, LIMIT_NS, || Ok(()))?;
    let napped = test_hooks::KILLED_NAPS.load(Ordering::Relaxed) - naps;
    check!(napped > 0, "the killed waiter never napped");
    Ok(())
}
