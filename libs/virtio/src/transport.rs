//! The mapped configuration structures of one modern virtio-PCI function:
//! status, feature negotiation, queue setup, notification and the ISR.
//!
//! All register access is volatile through pointers the caller mapped from the
//! device's BARs (uncached, so ordering is by program order). The constructor
//! is the only unsafe entry point; every later access is bounds-checked
//! against the lengths the device's own capabilities reported, so a hostile
//! notify multiplier or device-config offset cannot reach outside the mapping.

use core::cell::Cell;
use core::ptr;

use crate::queue::Virtqueue;
use crate::regs::{common, status, F_VERSION_1};
use crate::Error;

/// Register reads spent waiting for a reset to complete. Each read is one
/// MMIO exit, so this is milliseconds, not seconds.
const RESET_SPINS: u32 = 1_000_000;

/// Where to write to kick a queue: the queue index and the byte offset into
/// the notify structure, already checked against its length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kick {
    pub queue: u16,
    offset: u32,
}

pub struct Transport {
    common: *mut u8,
    notify: *mut u8,
    notify_len: u32,
    notify_multiplier: u32,
    isr: *mut u8,
    device: *mut u8,
    device_len: u32,
    /// The MSI-X table entry queues and configuration changes signal, or
    /// [`common::NO_VECTOR`] while the function interrupts with INTx.
    msix: Cell<u16>,
}

// SAFETY: the pointers are private MMIO mappings owned by this driver task.
unsafe impl Send for Transport {}

impl Transport {
    /// Wrap the mapped structures.
    ///
    /// # Safety
    /// * `common` must be valid for [`common::LEN`] bytes of volatile access,
    ///   `notify` for `notify_len`, `isr` for one byte, and `device` (when
    ///   non-null) for `device_len` bytes;
    /// * all must stay mapped for the transport's lifetime.
    pub unsafe fn new(
        common: *mut u8,
        notify: *mut u8,
        notify_len: u32,
        notify_multiplier: u32,
        isr: *mut u8,
        device: *mut u8,
        device_len: u32,
    ) -> Transport {
        Transport {
            common,
            notify,
            notify_len,
            notify_multiplier,
            isr,
            device,
            device_len: if device.is_null() { 0 } else { device_len },
            msix: Cell::new(common::NO_VECTOR),
        }
    }

    /// Signal configuration changes and every enabled queue through MSI-X
    /// table entry `vector`, and give it to queues set up later too. For a
    /// function the kernel switched to MSI-X (`irq_enable` answered MSI-X):
    /// with MSI-X on, a queue left at [`common::NO_VECTOR`] never interrupts.
    /// Fails when the device refuses the entry (it reads back `NO_VECTOR`).
    pub fn use_msix(&self, vector: u16) -> Result<(), Error> {
        self.msix.set(vector);
        self.w16(common::MSIX_CONFIG, vector);
        if self.r16(common::MSIX_CONFIG) != vector {
            return Err(Error::DeviceError);
        }
        for index in 0..self.num_queues() {
            self.w16(common::QUEUE_SELECT, index);
            if self.r16(common::QUEUE_ENABLE) == 0 {
                continue;
            }
            self.w16(common::QUEUE_MSIX_VECTOR, vector);
            if self.r16(common::QUEUE_MSIX_VECTOR) != vector {
                return Err(Error::DeviceError);
            }
        }
        Ok(())
    }

    fn r8(&self, offset: usize) -> u8 {
        // SAFETY: `offset < common::LEN` at every call site (constants).
        unsafe { ptr::read_volatile(self.common.add(offset)) }
    }

    fn r16(&self, offset: usize) -> u16 {
        // SAFETY: as `r8`; the register offsets are naturally aligned.
        u16::from_le(unsafe { ptr::read_volatile(self.common.add(offset) as *const u16) })
    }

    fn r32(&self, offset: usize) -> u32 {
        // SAFETY: as `r8`.
        u32::from_le(unsafe { ptr::read_volatile(self.common.add(offset) as *const u32) })
    }

    fn w8(&self, offset: usize, value: u8) {
        // SAFETY: as `r8`.
        unsafe { ptr::write_volatile(self.common.add(offset), value) }
    }

    fn w16(&self, offset: usize, value: u16) {
        // SAFETY: as `r8`.
        unsafe { ptr::write_volatile(self.common.add(offset) as *mut u16, value.to_le()) }
    }

    fn w32(&self, offset: usize, value: u32) {
        // SAFETY: as `r8`.
        unsafe { ptr::write_volatile(self.common.add(offset) as *mut u32, value.to_le()) }
    }

    /// 64-bit registers are written as two 32-bit halves, low first, which the
    /// spec allows for devices that do not take a 64-bit access.
    fn w64(&self, offset: usize, value: u64) {
        self.w32(offset, value as u32);
        self.w32(offset + 4, (value >> 32) as u32);
    }

    pub fn status(&self) -> u8 {
        self.r8(common::DEVICE_STATUS)
    }

    pub fn set_status(&self, bits: u8) {
        self.w8(common::DEVICE_STATUS, bits);
    }

    fn add_status(&self, bits: u8) {
        self.set_status(self.status() | bits);
    }

    /// Reset the device and wait until it reads back zero.
    pub fn reset(&self) -> Result<(), Error> {
        self.set_status(0);
        for _ in 0..RESET_SPINS {
            if self.status() == 0 {
                return Ok(());
            }
        }
        Err(Error::ResetTimeout)
    }

    /// The 64 feature bits the device offers.
    pub fn device_features(&self) -> u64 {
        self.w32(common::DEVICE_FEATURE_SELECT, 0);
        let low = self.r32(common::DEVICE_FEATURE);
        self.w32(common::DEVICE_FEATURE_SELECT, 1);
        let high = self.r32(common::DEVICE_FEATURE);
        u64::from(high) << 32 | u64::from(low)
    }

    fn set_driver_features(&self, features: u64) {
        self.w32(common::DRIVER_FEATURE_SELECT, 0);
        self.w32(common::DRIVER_FEATURE, features as u32);
        self.w32(common::DRIVER_FEATURE_SELECT, 1);
        self.w32(common::DRIVER_FEATURE, (features >> 32) as u32);
    }

    /// Run the initialization handshake up to `FEATURES_OK`.
    ///
    /// `required` bits must be offered (a driver that cannot work without them
    /// fails with [`Error::FeatureUnsupported`]); `wanted` bits are taken if
    /// offered. `VERSION_1` is always required: this crate only speaks the
    /// non-legacy interface. Returns the accepted feature set.
    pub fn negotiate(&self, required: u64, wanted: u64) -> Result<u64, Error> {
        self.reset()?;
        self.add_status(status::ACKNOWLEDGE);
        self.add_status(status::DRIVER);
        let offered = self.device_features();
        let required = required | F_VERSION_1;
        if offered & required != required {
            self.set_status(self.status() | status::FAILED);
            return Err(Error::FeatureUnsupported);
        }
        let accepted = required | (wanted & offered);
        self.set_driver_features(accepted);
        self.add_status(status::FEATURES_OK);
        if self.status() & status::FEATURES_OK == 0 {
            self.set_status(self.status() | status::FAILED);
            return Err(Error::FeaturesRejected);
        }
        Ok(accepted)
    }

    /// Tell the device the driver is ready; queues must be set up first.
    pub fn driver_ok(&self) -> Result<(), Error> {
        self.add_status(status::DRIVER_OK);
        if self.status() & (status::NEEDS_RESET | status::FAILED) != 0 {
            return Err(Error::DeviceError);
        }
        Ok(())
    }

    /// Number of virtqueues the device offers.
    pub fn num_queues(&self) -> u16 {
        self.r16(common::NUM_QUEUES)
    }

    /// Program queue `index` with `queue`'s rings and enable it. Returns the
    /// [`Kick`] that notifies it.
    pub fn setup_queue(&self, index: u16, queue: &Virtqueue) -> Result<Kick, Error> {
        self.setup_queue_at(
            index,
            queue.size(),
            queue.desc_bus(),
            queue.avail_bus(),
            queue.used_bus(),
        )
    }

    /// Program queue `index` with `size` entries whose descriptor table,
    /// available ring and used ring are at the given bus addresses, and enable
    /// it: for a driver that keeps its own split-queue memory (the kernel's
    /// virtio-blk, issue #497). Returns the [`Kick`] that notifies it.
    pub fn setup_queue_at(
        &self,
        index: u16,
        size: u16,
        desc: u64,
        avail: u64,
        used: u64,
    ) -> Result<Kick, Error> {
        if index >= self.num_queues() {
            return Err(Error::BadQueue);
        }
        self.w16(common::QUEUE_SELECT, index);
        let device_max = self.r16(common::QUEUE_SIZE);
        if device_max == 0 || size == 0 || size > device_max {
            return Err(Error::BadQueue);
        }
        self.w16(common::QUEUE_SIZE, size);
        // The entry `use_msix` chose, or none (INTx or polling).
        self.w16(common::QUEUE_MSIX_VECTOR, self.msix.get());
        self.w64(common::QUEUE_DESC, desc);
        self.w64(common::QUEUE_DRIVER, avail);
        self.w64(common::QUEUE_DEVICE, used);
        let notify_off = u32::from(self.r16(common::QUEUE_NOTIFY_OFF));
        let offset = notify_off
            .checked_mul(self.notify_multiplier)
            .filter(|offset| {
                offset
                    .checked_add(2)
                    .is_some_and(|end| end <= self.notify_len)
            })
            .ok_or(Error::BadQueue)?;
        self.w16(common::QUEUE_ENABLE, 1);
        Ok(Kick {
            queue: index,
            offset,
        })
    }

    /// The largest size the device supports for queue `index` (0 when the queue
    /// is not available). A driver picks its queue size at or below this, then
    /// calls [`Transport::setup_queue`].
    pub fn queue_max(&self, index: u16) -> Result<u16, Error> {
        if index >= self.num_queues() {
            return Err(Error::BadQueue);
        }
        self.w16(common::QUEUE_SELECT, index);
        Ok(self.r16(common::QUEUE_SIZE))
    }

    /// Notify the device that `kick`'s queue has new buffers.
    pub fn notify(&self, kick: Kick) {
        // SAFETY: `setup_queue` checked `offset + 2 <= notify_len`, and the
        // notify address is 2-byte aligned for the multipliers the spec allows
        // (a multiple of 2); a misaligned one only costs a slower access.
        unsafe {
            ptr::write_volatile(
                self.notify.add(kick.offset as usize) as *mut u16,
                kick.queue.to_le(),
            )
        }
    }

    /// Read and clear the ISR status (bit 0 queue interrupt, bit 1 config).
    pub fn isr_status(&self) -> u8 {
        // SAFETY: the constructor guarantees one readable ISR byte.
        unsafe { ptr::read_volatile(self.isr) }
    }

    /// Read `width` (1, 2 or 4) bytes of device-specific configuration.
    pub fn device_config(&self, offset: u32, width: u32) -> Result<u32, Error> {
        if !matches!(width, 1 | 2 | 4) || !offset.is_multiple_of(width) {
            return Err(Error::BadRequest);
        }
        if offset
            .checked_add(width)
            .is_none_or(|end| end > self.device_len)
        {
            return Err(Error::BadRequest);
        }
        // SAFETY: bounds checked against `device_len` just above.
        let base = unsafe { self.device.add(offset as usize) };
        // SAFETY: as above, and naturally aligned by the modulus check.
        Ok(unsafe {
            match width {
                1 => u32::from(ptr::read_volatile(base)),
                2 => u32::from(u16::from_le(ptr::read_volatile(base as *const u16))),
                _ => u32::from_le(ptr::read_volatile(base as *const u32)),
            }
        })
    }
}

#[cfg(test)]
mod tests;
