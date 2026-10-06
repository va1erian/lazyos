//! The in-kernel virtio-blk driver (issues #100, #497).
//!
//! A function is driven through the modern (virtio 1.x) transport whenever it
//! has the virtio PCI capabilities, a transitional `1af4:1001` or a
//! modern-only `1af4:1042`, through the same `libs/virtio` transport code as
//! the userspace drivers ([`modern`]); a legacy-only function (QEMU's
//! `disable-modern=on`) through its virtio 0.9.5 I/O window ([`io`]). Either
//! way the driver negotiates no feature but the transport's own and sets up
//! queue 0 as a split virtqueue in a static, physically contiguous region;
//! [`regs::Regs`] is the only place the two differ.
//!
//! Each attached function owns one [`Slot`]: its queue, one control block
//! (request header and status byte) per request slot, and the registry-facing
//! device. The device core offers functions one at a time
//! ([`attach_function`]), so a boot disk and a data disk on separate
//! virtio-blk functions are two independent block devices (`virtio0`,
//! `virtio1`, ...) that never share queue state.
//!
//! # Requests (docs/performance-plan.md P5)
//!
//! A transfer is cut into requests of up to 256 KiB ([`plan`]); up to
//! [`MAX_INFLIGHT`] requests, from any callers, are in the queue at once
//! (`ring.rs`). The device reads and writes the caller's buffers directly,
//! one descriptor per page piece translated with [`super::virt_to_phys`]:
//! there is no bounce copy. The device's lock is held only to submit and to
//! reap, never while waiting, and a waiter that may sleep parks
//! ([`super::iowait`]) while the device works.
//!
//! A transfer never returns while the device may still touch its buffers: it
//! waits for every request it submitted, and a request that does not complete
//! within [`TIMEOUT_NS`] resets the device (after which it touches no memory),
//! failing every request then in flight.
//!
mod io;
mod modern;
pub mod plan;
mod queue;
mod regs;
mod ring;

use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::Ordering;
use spin::Mutex;
use x86_64::VirtAddr;

use super::iowait::{self, Expect};
use super::stats::IoStats;
use super::virtio_diag as diag;
use super::{BlockDevice, BlockError, Wait, SECTOR_SIZE};
pub use io::attach_function;
use plan::Cursor;
use queue::{Control, ControlCell, Queue, QUEUE_BYTES};
use regs::Regs;
/// How a driven function is reached, for the failure reports.
pub(super) use regs::Regs as DeviceRegs;
use ring::{Hints, Ring, MAX_INFLIGHT};

/// A request that has not completed after this long resets the device.
const TIMEOUT_NS: u64 = 10_000_000_000;
/// Bytes in one request (fewer when the buffers need more than
/// [`plan::MAX_PIECES`] page pieces).
pub const MAX_REQUEST_BYTES: usize = 256 * 1024;

/// How many virtio-blk functions can be driven at once (one [`Slot`] each).
const MAX_VIRTIO: usize = 4;
/// Registry names, indexed by slot.
const NAMES: [&str; MAX_VIRTIO] = ["virtio0", "virtio1", "virtio2", "virtio3"];

/// Everything discovered at attach, and the queue's bookkeeping.
struct State {
    regs: Regs,
    sectors: u64,
    ring: Ring,
    /// Physical address of each request slot's control block.
    control_phys: [u64; MAX_INFLIGHT],
}

/// One driven function: its DMA memory and the registry-facing device. The
/// device's state is `None` until [`attach_function`] claims the slot.
#[repr(C)]
struct Slot {
    queue: Queue,
    controls: [ControlCell; MAX_INFLIGHT],
    device: VirtioBlk,
}

/// The registry-facing device of one [`Slot`].
pub struct VirtioBlk {
    /// Index into [`SLOTS`]; names the device and finds its DMA memory.
    index: usize,
    state: Mutex<Option<State>>,
    hints: Hints,
    /// How long reads and writes usually take, for a waiter's first look.
    expect_read: Expect,
    expect_write: Expect,
    stats: IoStats,
}

impl Slot {
    const fn new(index: usize) -> Slot {
        Slot {
            queue: Queue(UnsafeCell::new([0; QUEUE_BYTES])),
            controls: [const {
                ControlCell(UnsafeCell::new(Control {
                    header: [0; 16],
                    status: 0,
                }))
            }; MAX_INFLIGHT],
            device: VirtioBlk {
                index,
                state: Mutex::new(None),
                hints: Hints::new(),
                expect_read: Expect::new(),
                expect_write: Expect::new(),
                stats: IoStats::new(),
            },
        }
    }
}

static SLOTS: [Slot; MAX_VIRTIO] = [Slot::new(0), Slot::new(1), Slot::new(2), Slot::new(3)];

/// One transfer: its segments, how far it got, and its requests in the queue.
struct Transfer<'a> {
    write: bool,
    lba: u64,
    segments: &'a [(u64, usize)],
    total: usize,
    cursor: Cursor,
    submitted: usize,
    /// Request slot -> the queue epoch it was submitted in.
    mine: [Option<u64>; MAX_INFLIGHT],
    result: Result<(), BlockError>,
}

impl Transfer<'_> {
    fn in_flight(&self) -> bool {
        self.mine.iter().any(Option::is_some)
    }

    fn mask(&self) -> u32 {
        (0..MAX_INFLIGHT)
            .filter(|&slot| self.mine[slot].is_some())
            .fold(0, |mask, slot| mask | 1 << slot)
    }
}

/// What a wait compares the lock-free hints against.
#[derive(Clone, Copy)]
struct Snapshot {
    epoch: u64,
    released: u64,
    used_off: usize,
}

impl VirtioBlk {
    fn slot(&self) -> &'static Slot {
        &SLOTS[self.index]
    }

    /// Move the bytes of `segments` (`(address, length)`, back to back)
    /// to (`write`) or from the device at `lba`.
    fn transfer(
        &self,
        write: bool,
        lba: u64,
        segments: &[(u64, usize)],
        wait: Wait,
    ) -> Result<(), BlockError> {
        let total: usize = segments.iter().map(|&(_, len)| len).sum();
        super::check_range(SECTOR_SIZE, self.sector_count(), lba, total)?;
        if total == 0 {
            return Ok(());
        }
        let expect = if write {
            &self.expect_write
        } else {
            &self.expect_read
        };
        let mut transfer = Transfer {
            write,
            lba,
            segments,
            total,
            cursor: Cursor::default(),
            submitted: 0,
            mine: [None; MAX_INFLIGHT],
            result: Ok(()),
        };
        loop {
            let Some(snapshot) = self.step(&mut transfer) else {
                // Not attached (or detached by a failed reset): nothing of
                // ours can be in the queue.
                return Err(BlockError::Io);
            };
            let finished = transfer.submitted == transfer.total || transfer.result.is_err();
            if !transfer.in_flight() && finished {
                return transfer.result;
            }
            let mask = transfer.mask();
            let hints = &self.hints;
            let queue = &self.slot().queue;
            let progressed = || {
                hints.done.load(Ordering::Acquire) & mask != 0
                    || hints.epoch.load(Ordering::Acquire) != snapshot.epoch
                    || hints.released.load(Ordering::Acquire) != snapshot.released
                    || Ring::device_used(queue, snapshot.used_off)
                        != hints.reaped.load(Ordering::Acquire)
            };
            if !iowait::wait_until(wait, expect, TIMEOUT_NS, progressed) {
                self.reset_after_timeout(write, lba, total);
                // The reset failed every request in flight, ours included:
                // the device will not touch our buffers again.
                return Err(BlockError::Io);
            }
        }
    }

    /// Under the lock: reap, take our finished requests, submit what fits.
    /// Returns what the following wait compares against, or `None` when the
    /// device is not attached.
    fn step(&self, transfer: &mut Transfer) -> Option<Snapshot> {
        let slot = self.slot();
        let mut guard = self.state.lock();
        let state = guard.as_mut()?;
        let stats = &self.stats;
        state
            .ring
            .reap(&slot.queue, &slot.controls, &self.hints, |write, bytes| {
                stats.count(write, bytes);
            });
        let epoch = self.hints.epoch.load(Ordering::Acquire);
        self.take_finished(state, transfer, epoch);
        let mut published = false;
        while transfer.result.is_ok() && transfer.submitted < transfer.total {
            let Some(free) = state.ring.free_slot() else {
                break;
            };
            let plan = match plan::plan(
                transfer.segments,
                transfer.cursor,
                MAX_REQUEST_BYTES,
                |virt| super::virt_to_phys(VirtAddr::new(virt)).map(|phys| phys.as_u64()),
            ) {
                Ok(plan) => plan,
                Err(_) => {
                    // Unmapped or sub-sector buffers: nothing more is sent.
                    transfer.result = Err(BlockError::Unsupported);
                    break;
                }
            };
            if !state.ring.fits(&plan) {
                break;
            }
            let at = transfer.lba + (transfer.submitted / SECTOR_SIZE) as u64;
            // SAFETY: the queue and control block are this device's, slot
            // `free` is unused, the lock is held, and the plan's buffers are
            // the caller's segments, which outlive this transfer: it returns
            // only once every request it submitted completed or the device
            // was reset.
            unsafe {
                state.ring.submit(
                    &slot.queue,
                    &slot.controls[free],
                    state.control_phys[free],
                    free,
                    transfer.write,
                    at,
                    &plan,
                );
            }
            // A stale completion bit of the slot's previous request must not
            // wake this one's waiter.
            self.hints.done.fetch_and(!(1 << free), Ordering::AcqRel);
            transfer.mine[free] = Some(epoch);
            transfer.cursor.advance(transfer.segments, plan.bytes);
            transfer.submitted += plan.bytes;
            published = true;
        }
        if published {
            core::sync::atomic::fence(Ordering::Release);
            state.regs.notify();
        }
        Some(Snapshot {
            epoch,
            released: self.hints.released.load(Ordering::Acquire),
            used_off: state.ring.used_off,
        })
    }

    /// Take the results of `transfer`'s requests that finished (or that a
    /// reset failed) and free their slots.
    fn take_finished(&self, state: &mut State, transfer: &mut Transfer, epoch: u64) {
        for index in 0..MAX_INFLIGHT {
            let Some(mine) = transfer.mine[index] else {
                continue;
            };
            if mine != epoch {
                // A reset failed it; the slot may already be someone else's.
                transfer.mine[index] = None;
                transfer.result = Err(BlockError::Io);
                continue;
            }
            let Some(entry) = state.ring.inflight[index] else {
                continue;
            };
            if !entry.done {
                continue;
            }
            if entry.status != 0 {
                diag::log(
                    &state.regs,
                    entry.write,
                    entry.lba,
                    entry.bytes,
                    "status",
                    u64::from(entry.status),
                );
                transfer.result = Err(BlockError::Io);
            }
            state.ring.release(index, &self.hints);
            transfer.mine[index] = None;
        }
    }

    /// A request outlived [`TIMEOUT_NS`]: reset the device so it lets go of
    /// every buffer, fail all requests then in flight, and set the queue up
    /// again for the next caller.
    fn reset_after_timeout(&self, write: bool, lba: u64, bytes: usize) {
        if let Some(state) = self.state.lock().as_ref() {
            diag::log(&state.regs, write, lba, bytes, "timeout; device reset", 0);
        }
        self.reset_device();
    }

    /// Reset the device and set its queue up again; every request in flight
    /// fails. A device that does not come back is detached.
    fn reset_device(&self) {
        let mut guard = self.state.lock();
        let Some(state) = guard.as_mut() else {
            return;
        };
        self.hints.epoch.fetch_add(1, Ordering::AcqRel);
        // SAFETY: `state.regs` is this slot's device and the lock is held, so
        // nobody else touches the queue; after the reset write the device
        // touches no guest memory.
        match unsafe { state.regs.reset(self.slot()) } {
            Some(ring) => state.ring = ring,
            None => *guard = None, // the device did not come back: detached
        }
        // Every waiter sees the new epoch and fails its requests; no slot of
        // the fresh queue is done.
        self.hints.done.store(0, Ordering::Release);
        self.hints.reaped.store(0, Ordering::Release);
        self.hints.released.fetch_add(1, Ordering::AcqRel);
    }
}

/// The driven function named `name`, for the kernel suite.
#[cfg(lazyos_tests)]
fn by_name(name: &str) -> Option<&'static VirtioBlk> {
    SLOTS
        .iter()
        .map(|slot| &slot.device)
        .find(|device| NAMES[device.index] == name)
}

/// How the device `name` is driven (`"modern"`, `"legacy io 0x..."`), for the
/// kernel suite (issue #497).
#[cfg(lazyos_tests)]
pub fn transport_of(name: &str) -> Option<alloc::string::String> {
    by_name(name)?
        .state
        .lock()
        .as_ref()
        .map(|state| state.regs.describe())
}

/// Put the device `name` through the reset a timed-out request causes, for
/// the kernel suite; false when it is not attached afterwards.
#[cfg(lazyos_tests)]
pub fn reset_for_test(name: &str) -> bool {
    let Some(device) = by_name(name) else {
        return false;
    };
    device.reset_device();
    device.state.lock().is_some()
}

/// `(address, length)` of each buffer.
fn spans(bufs: impl Iterator<Item = (*const u8, usize)>) -> Result<Vec<(u64, usize)>, BlockError> {
    let mut out = Vec::new();
    for (ptr, len) in bufs {
        out.try_reserve(1).map_err(|_| BlockError::Io)?;
        out.push((ptr as u64, len));
    }
    Ok(out)
}

impl BlockDevice for VirtioBlk {
    fn name(&self) -> &'static str {
        NAMES[self.index]
    }

    fn sector_count(&self) -> u64 {
        self.state.lock().as_ref().map_or(0, |state| state.sectors)
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), BlockError> {
        self.read_sectors_vectored_with(lba, &mut [buf], Wait::Spin)
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), BlockError> {
        self.write_sectors_vectored_with(lba, &[buf], Wait::Spin)
    }

    fn read_sectors_vectored(&self, lba: u64, bufs: &mut [&mut [u8]]) -> Result<(), BlockError> {
        self.read_sectors_vectored_with(lba, bufs, Wait::Spin)
    }

    fn write_sectors_vectored(&self, lba: u64, bufs: &[&[u8]]) -> Result<(), BlockError> {
        self.write_sectors_vectored_with(lba, bufs, Wait::Spin)
    }

    /// The device writes straight into `bufs`, which stay borrowed until
    /// every request completed (or the device was reset).
    fn read_sectors_vectored_with(
        &self,
        lba: u64,
        bufs: &mut [&mut [u8]],
        wait: Wait,
    ) -> Result<(), BlockError> {
        let segments = spans(
            bufs.iter_mut()
                .map(|buf| (buf.as_mut_ptr() as *const u8, buf.len())),
        )?;
        self.transfer(false, lba, &segments, wait)
    }

    fn write_sectors_vectored_with(
        &self,
        lba: u64,
        bufs: &[&[u8]],
        wait: Wait,
    ) -> Result<(), BlockError> {
        let segments = spans(bufs.iter().map(|buf| (buf.as_ptr(), buf.len())))?;
        self.transfer(true, lba, &segments, wait)
    }

    fn is_writable(&self) -> bool {
        self.state.lock().is_some()
    }

    fn stats(&self) -> Option<&IoStats> {
        Some(&self.stats)
    }

    fn flush(&self) -> Result<(), BlockError> {
        // No feature bits are negotiated, so the device advertises no write
        // cache to flush; a completed request is already ordered by QEMU.
        Ok(())
    }
}
