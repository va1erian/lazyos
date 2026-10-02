//! Per-device I/O counters and the quiet-period report.
//!
//! A driver counts every request it sends to the hardware (one virtio request,
//! however many sectors it carries), so the numbers measure what costs time
//! under a hypervisor: exits, not bytes. [`service`] runs from the kernel
//! task's loop and prints one `block:` line per device after a burst of I/O
//! has gone quiet, which is how a harness reads the cost of, say, first-boot
//! provisioning off the serial log without a new syscall.

use core::sync::atomic::{AtomicU64, Ordering};

use super::BlockDevice;

/// Request counters of one device. Relaxed atomics: they are statistics, read
/// as a snapshot, and never order anything.
pub struct IoStats {
    reads: AtomicU64,
    read_bytes: AtomicU64,
    writes: AtomicU64,
    write_bytes: AtomicU64,
    flushes: AtomicU64,
}

/// A copy of [`IoStats`] at one moment.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub struct Snapshot {
    pub reads: u64,
    pub read_bytes: u64,
    pub writes: u64,
    pub write_bytes: u64,
    pub flushes: u64,
}

impl Snapshot {
    /// Requests of any kind; what [`service`] watches for activity.
    pub fn requests(&self) -> u64 {
        self.reads + self.writes + self.flushes
    }
}

impl IoStats {
    pub const fn new() -> IoStats {
        IoStats {
            reads: AtomicU64::new(0),
            read_bytes: AtomicU64::new(0),
            writes: AtomicU64::new(0),
            write_bytes: AtomicU64::new(0),
            flushes: AtomicU64::new(0),
        }
    }

    /// One request of `bytes` reached the device.
    pub fn count(&self, write: bool, bytes: usize) {
        let (requests, total) = if write {
            (&self.writes, &self.write_bytes)
        } else {
            (&self.reads, &self.read_bytes)
        };
        requests.fetch_add(1, Ordering::Relaxed);
        total.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// One cache flush reached the device.
    #[allow(dead_code)] // no driver negotiates a write cache yet
    pub fn count_flush(&self) {
        self.flushes.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            reads: self.reads.load(Ordering::Relaxed),
            read_bytes: self.read_bytes.load(Ordering::Relaxed),
            writes: self.writes.load(Ordering::Relaxed),
            write_bytes: self.write_bytes.load(Ordering::Relaxed),
            flushes: self.flushes.load(Ordering::Relaxed),
        }
    }
}

/// How long a device must stay idle after activity before it is reported
/// (100 Hz ticks): long enough that one report covers one burst.
const QUIET_TICKS: u64 = 300;

/// What [`service`] remembers between calls: per registry slot, the request
/// count last seen, when it last changed, and the count last reported.
struct Watch {
    seen: u64,
    changed_at: u64,
    reported: u64,
}

static WATCH: spin::Mutex<[Watch; 4]> = spin::Mutex::new(
    [const {
        Watch {
            seen: 0,
            changed_at: 0,
            reported: 0,
        }
    }; 4],
);

/// Print each whole disk's counters once its burst has been quiet for
/// [`QUIET_TICKS`]. Cheap when nothing changed; never blocks on a busy lock.
pub fn service() {
    let Some(mut watch) = WATCH.try_lock() else {
        return;
    };
    let now = crate::task::ticks();
    let disks = super::devices();
    let counted = disks.iter().filter_map(|disk| Some((*disk, disk.stats()?)));
    for (slot, (disk, stats)) in watch.iter_mut().zip(counted) {
        let snap = stats.snapshot();
        if snap.requests() != slot.seen {
            slot.seen = snap.requests();
            slot.changed_at = now;
        } else if slot.seen != slot.reported && now.wrapping_sub(slot.changed_at) >= QUIET_TICKS {
            slot.reported = slot.seen;
            report(disk, &snap);
        }
    }
}

/// One `block:` line; harnesses read the cost of a run from it.
pub fn report(disk: &dyn BlockDevice, snap: &Snapshot) {
    serial_println!(
        "block: {} reads {} ({} KiB) writes {} ({} KiB) flushes {}",
        disk.name(),
        snap.reads,
        snap.read_bytes / 1024,
        snap.writes,
        snap.write_bytes / 1024,
        snap.flushes
    );
}
