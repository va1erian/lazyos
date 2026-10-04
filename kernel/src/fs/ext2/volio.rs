//! The volume's disk as the library sees it, and the gate every call into the
//! library passes (docs/performance-plan.md P5).
//!
//! Whoever holds the gate holds no plain spin lock another task could want
//! without first meeting a yielding one: the gate itself and both mount
//! tables are `task::relax::YieldMutex`es, and the library's own spin locks
//! are only reached through the gate. So while the gate is held, a request
//! may sleep ([`Wait::MaySleep`]): [`Entered`] says so to [`VolumeIo`] for as
//! long as it lives, and the block driver parks the caller instead of
//! spinning with interrupts off (`block::iowait`).

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};

use super::{io_error, Ext2};
use crate::block::{iowait, BlockDevice, Wait};
use crate::task::relax::YieldMutex;

/// A registered device, plus whether the gate holder may sleep in it.
pub(super) struct VolumeIo {
    pub(super) device: &'static dyn BlockDevice,
    may_sleep: AtomicBool,
}

impl VolumeIo {
    pub(super) fn new(device: &'static dyn BlockDevice) -> Arc<VolumeIo> {
        Arc::new(VolumeIo {
            device,
            may_sleep: AtomicBool::new(false),
        })
    }

    /// How the current gate holder's requests wait.
    pub(super) fn wait(&self) -> Wait {
        if self.may_sleep.load(Ordering::Relaxed) {
            Wait::MaySleep
        } else {
            Wait::Spin
        }
    }
}

/// The gate, held: the library may be called, and its requests may sleep.
pub(super) struct Entered<'a> {
    _gate: spin::mutex::MutexGuard<'a, ()>,
    io: &'a VolumeIo,
}

impl Entered<'_> {
    /// Between two pieces of a long call (the library's lock released): let
    /// interrupts in and the scheduler act (`iowait::breathe`).
    pub(super) fn breathe(&self) {
        iowait::breathe(self.io.wait());
    }
}

impl Drop for Entered<'_> {
    fn drop(&mut self) {
        self.io.may_sleep.store(false, Ordering::Relaxed);
    }
}

impl Ext2 {
    /// Take the gate (yielding while another task holds it).
    pub(super) fn enter(&self) -> Entered<'_> {
        let gate = self.gate.lock();
        self.io.may_sleep.store(true, Ordering::Relaxed);
        Entered {
            _gate: gate,
            io: &self.io,
        }
    }

    /// [`Ext2::enter`] unless another task holds the gate.
    pub(super) fn try_enter(&self) -> Option<Entered<'_>> {
        let gate = self.gate.try_lock()?;
        self.io.may_sleep.store(true, Ordering::Relaxed);
        Some(Entered {
            _gate: gate,
            io: &self.io,
        })
    }
}

/// The library's pause hook (`ext2fs::Ext2::set_pause`): inside a long
/// operation, with the volume lock held, breathe as between two pieces. Only
/// when the gate's holder may sleep, which is exactly when that lock is only
/// reachable through the (yielding) gate.
pub(super) fn pause_hook(io: &Arc<VolumeIo>) -> alloc::boxed::Box<dyn Fn() + Send + Sync> {
    let io = io.clone();
    alloc::boxed::Box::new(move || iowait::breathe(io.wait()))
}

/// The handle the library owns; the volume keeps another to the same
/// [`VolumeIo`].
pub(super) struct SharedIo(pub(super) Arc<VolumeIo>);

/// A registered device is the library's disk. The explicit `BlockDevice::`
/// calls name the trait the device implements.
///
/// Every transfer first collects the i8042's bytes (`input::ps2`): a file
/// syscall that spins runs with interrupts off and may do hundreds of these,
/// long enough for the keyboard controller's own queue to overflow.
impl ext2fs::BlockIo for SharedIo {
    fn sector_count(&self) -> u64 {
        BlockDevice::sector_count(self.0.device)
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), ext2fs::IoError> {
        self.read_sectors_vectored(lba, &mut [buf])
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), ext2fs::IoError> {
        self.write_sectors_vectored(lba, &[buf])
    }

    fn read_sectors_vectored(
        &self,
        lba: u64,
        bufs: &mut [&mut [u8]],
    ) -> Result<(), ext2fs::IoError> {
        crate::input::ps2::service();
        BlockDevice::read_sectors_vectored_with(self.0.device, lba, bufs, self.0.wait())
            .map_err(io_error)
    }

    fn write_sectors_vectored(&self, lba: u64, bufs: &[&[u8]]) -> Result<(), ext2fs::IoError> {
        crate::input::ps2::service();
        BlockDevice::write_sectors_vectored_with(self.0.device, lba, bufs, self.0.wait())
            .map_err(io_error)
    }

    fn flush(&self) -> Result<(), ext2fs::IoError> {
        BlockDevice::flush(self.0.device).map_err(io_error)
    }

    fn is_writable(&self) -> bool {
        BlockDevice::is_writable(self.0.device)
    }
}

/// The gate's type, named once.
pub(super) type Gate = YieldMutex<()>;

/// A bare registered device as a library disk, always spinning: what the
/// test suites format and open directly, outside any gate.
impl ext2fs::BlockIo for &'static dyn BlockDevice {
    fn sector_count(&self) -> u64 {
        BlockDevice::sector_count(*self)
    }

    fn read_sectors(&self, lba: u64, buf: &mut [u8]) -> Result<(), ext2fs::IoError> {
        crate::input::ps2::service();
        BlockDevice::read_sectors(*self, lba, buf).map_err(io_error)
    }

    fn write_sectors(&self, lba: u64, buf: &[u8]) -> Result<(), ext2fs::IoError> {
        crate::input::ps2::service();
        BlockDevice::write_sectors(*self, lba, buf).map_err(io_error)
    }

    fn read_sectors_vectored(
        &self,
        lba: u64,
        bufs: &mut [&mut [u8]],
    ) -> Result<(), ext2fs::IoError> {
        crate::input::ps2::service();
        BlockDevice::read_sectors_vectored(*self, lba, bufs).map_err(io_error)
    }

    fn write_sectors_vectored(&self, lba: u64, bufs: &[&[u8]]) -> Result<(), ext2fs::IoError> {
        crate::input::ps2::service();
        BlockDevice::write_sectors_vectored(*self, lba, bufs).map_err(io_error)
    }

    fn flush(&self) -> Result<(), ext2fs::IoError> {
        BlockDevice::flush(*self).map_err(io_error)
    }

    fn is_writable(&self) -> bool {
        BlockDevice::is_writable(*self)
    }
}
