//! Kernel threads parked in the ATA PIO driver while the drive is busy
//! (issue #449), next to a thread that spins its reads through the same
//! channel with interrupts off, as a syscall does. Runs on the IDE boot disk
//! (`tools/test/run.py --ide-disk`) and reports a skip without one.
//!
//! The spinner is a thread, not the suite's kernel task: the harness never
//! preempts the kernel task, so a kernel task contending the channel lock a
//! parked reader holds would wait forever, while a contending thread yields
//! to it (`task::relax`), exactly as a contending syscall does on a running
//! system.

use super::*;
use crate::block::iowait::test_hooks;
use crate::block::{Wait, SECTOR_SIZE};

/// Threads in all; the first to start spins, the others sleep.
const THREADS: usize = 4;
const ROUNDS: usize = 40;
const LIMIT_NS: u64 = 300_000_000_000;
/// Reads stay inside the first 32 MiB, which every image has.
const SPAN: u64 = 64 * 1024;

/// Which role the next thread takes (0: the spinner).
static NEXT_ROLE: AtomicUsize = AtomicUsize::new(0);
/// Round trips the spinner finished.
static SPINS: AtomicUsize = AtomicUsize::new(0);

/// The primary IDE master, if the machine has one.
fn ata(test: &str) -> Option<&'static dyn BlockDevice> {
    let found = block::devices()
        .into_iter()
        .find(|device| device.name() == "ata0");
    if found.is_none() {
        serial_println!("TEST:{test}:INFO:no IDE disk (run with --ide-disk); skipped");
    }
    found
}

/// Read a random extent split in two with a request that waits as `wait`
/// says, then again with a plain spinning read: both must see the same bytes.
fn round_trip(
    disk: &dyn BlockDevice,
    rng: &mut Rng,
    round: usize,
    wait: Wait,
) -> Result<(), String> {
    let span = SPAN.min(disk.sector_count());
    let sectors = 1 + rng.below(300);
    let lba = rng.below(span - sectors);
    let len = sectors as usize * SECTOR_SIZE;
    let mut first = vec![0u8; len];
    {
        let split = rng.below(sectors) as usize * SECTOR_SIZE;
        let (head, tail) = first.split_at_mut(split);
        disk.read_sectors_vectored_with(lba, &mut [head, tail], wait)
            .map_err(|e| format!("round {round}: {wait:?} read {lba}+{sectors}: {e:?}"))?;
    }
    let mut second = vec![0u8; len];
    disk.read_sectors(lba, &mut second)
        .map_err(|e| format!("round {round}: spinning read {lba}+{sectors}: {e:?}"))?;
    check!(
        first == second,
        "round {round}: {lba}+{sectors} read differently ({wait:?}, then spinning)"
    );
    Ok(())
}

/// The spinner's rounds run with interrupts off, like a syscall's.
fn spinning_round(disk: &dyn BlockDevice, rng: &mut Rng, round: usize) -> Result<(), String> {
    let enabled = x86_64::instructions::interrupts::are_enabled();
    x86_64::instructions::interrupts::disable();
    let result = round_trip(disk, rng, round, Wait::Spin);
    if enabled {
        x86_64::instructions::interrupts::enable();
    }
    SPINS.fetch_add(1, Ordering::Relaxed);
    result
}

extern "C" fn reader() -> ! {
    let spinner = NEXT_ROLE.fetch_add(1, Ordering::Relaxed) == 0;
    let mut rng = Rng::seeded();
    let result = match ata("block_sleep_ata_threads") {
        Some(disk) if spinner => {
            (0..ROUNDS).try_for_each(|round| spinning_round(disk, &mut rng, round))
        }
        Some(disk) => {
            (0..ROUNDS).try_for_each(|round| round_trip(disk, &mut rng, round, Wait::MaySleep))
        }
        None => Ok(()),
    };
    finish_thread(result)
}

/// Soak: three threads read the IDE disk with requests that park while the
/// drive is busy, while a fourth spins its reads with interrupts off through
/// the same channel: every byte agrees, the readers did sleep, and nobody
/// deadlocked on the channel lock a parked reader holds.
pub fn threads_and_a_spinner() -> Result<(), String> {
    if ata("block_sleep_ata_threads").is_none() {
        return Ok(());
    }
    NEXT_ROLE.store(0, Ordering::Relaxed);
    SPINS.store(0, Ordering::Relaxed);
    let parks = test_hooks::PARKS.load(Ordering::Relaxed);
    run_threads(THREADS, reader, LIMIT_NS, || Ok(()))?;
    let slept = test_hooks::PARKS.load(Ordering::Relaxed) - parks;
    let spins = SPINS.load(Ordering::Relaxed);
    serial_println!(
        "TEST:block_sleep_ata_threads:INFO:{} round trips, {slept} parks, {spins} spinning round trips",
        THREADS * ROUNDS
    );
    check!(slept > 0, "no ATA request ever slept");
    check!(
        spins == ROUNDS,
        "the spinner finished {spins} of {ROUNDS} rounds"
    );
    Ok(())
}
