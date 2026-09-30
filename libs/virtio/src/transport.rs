//! The mapped configuration structures of one modern virtio-PCI function:
//! status, feature negotiation, queue setup, notification and the ISR.
//!
//! All register access is volatile through pointers the caller mapped from the
//! device's BARs (uncached, so ordering is by program order). The constructor
//! is the only unsafe entry point; every later access is bounds-checked
//! against the lengths the device's own capabilities reported, so a hostile
//! notify multiplier or device-config offset cannot reach outside the mapping.

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
        }
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
        if index >= self.num_queues() {
            return Err(Error::BadQueue);
        }
        self.w16(common::QUEUE_SELECT, index);
        let device_max = self.r16(common::QUEUE_SIZE);
        if device_max == 0 || queue.size() > device_max {
            return Err(Error::BadQueue);
        }
        self.w16(common::QUEUE_SIZE, queue.size());
        // No MSI-X: the driver polls or takes INTx.
        self.w16(common::QUEUE_MSIX_VECTOR, common::NO_VECTOR);
        self.w64(common::QUEUE_DESC, queue.desc_bus());
        self.w64(common::QUEUE_DRIVER, queue.avail_bus());
        self.w64(common::QUEUE_DEVICE, queue.used_bus());
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
mod tests {
    use super::*;
    use std::vec::Vec;

    /// Plain memory standing in for the BARs. It has no device behind it, so
    /// the tests preload what the "device" would answer and check what the
    /// driver wrote.
    struct Rig {
        common: Vec<u64>,
        notify: Vec<u64>,
        isr: Vec<u64>,
        device: Vec<u64>,
    }

    impl Rig {
        fn new() -> Rig {
            Rig {
                common: std::vec![0; 16],
                notify: std::vec![0; 16],
                isr: std::vec![0; 1],
                device: std::vec![0; 4],
            }
        }

        fn transport(&mut self, multiplier: u32, notify_len: u32) -> Transport {
            // SAFETY: the buffers outlive the transport in each test and are
            // large enough for the lengths passed.
            unsafe {
                Transport::new(
                    self.common.as_mut_ptr() as *mut u8,
                    self.notify.as_mut_ptr() as *mut u8,
                    notify_len,
                    multiplier,
                    self.isr.as_mut_ptr() as *mut u8,
                    self.device.as_mut_ptr() as *mut u8,
                    32,
                )
            }
        }

        fn set16(&mut self, offset: usize, value: u16) {
            let bytes = self.common.as_mut_ptr() as *mut u8;
            // SAFETY: test offsets are inside the 128-byte buffer.
            unsafe { ptr::write(bytes.add(offset) as *mut u16, value) }
        }

        fn get(&self, offset: usize) -> u32 {
            let bytes = self.common.as_ptr() as *const u8;
            // SAFETY: as above.
            unsafe { ptr::read(bytes.add(offset) as *const u32) }
        }
    }

    fn queue_block(size: u16) -> (Vec<u32>, Virtqueue) {
        let mut block = std::vec![0u32; Virtqueue::bytes_needed(size).div_ceil(4)];
        // SAFETY: sized and aligned; kept alive next to the queue.
        let queue = unsafe { Virtqueue::new(block.as_mut_ptr() as *mut u8, 0x2_0000_1000, size) }
            .expect("queue");
        (block, queue)
    }

    #[test]
    fn negotiate_accepts_version_1_and_wanted_subset() {
        let mut rig = Rig::new();
        // The device answers every feature-select with 0x1 (low bit and, in
        // the high word, VERSION_1).
        let bytes = rig.common.as_mut_ptr() as *mut u8;
        // SAFETY: offset 4 is inside the buffer.
        unsafe { ptr::write(bytes.add(4) as *mut u32, 0x1) };
        let transport = rig.transport(4, 64);
        let accepted = transport.negotiate(0, 0x1).expect("negotiate");
        assert_eq!(accepted, F_VERSION_1 | 0x1);
        assert_eq!(
            rig.get(common::DEVICE_STATUS) as u8 & status::FEATURES_OK,
            status::FEATURES_OK
        );
    }

    #[test]
    fn negotiate_fails_without_a_required_feature() {
        let mut rig = Rig::new();
        let bytes = rig.common.as_mut_ptr() as *mut u8;
        // SAFETY: inside the buffer.
        unsafe { ptr::write(bytes.add(4) as *mut u32, 0) }; // offers nothing
        let transport = rig.transport(4, 64);
        assert_eq!(transport.negotiate(0, 0), Err(Error::FeatureUnsupported));
        assert_ne!(transport.status() & status::FAILED, 0);
    }

    #[test]
    fn setup_queue_programs_addresses_and_checks_notify_bounds() {
        let mut rig = Rig::new();
        rig.set16(common::NUM_QUEUES, 4);
        rig.set16(common::QUEUE_SIZE, 256);
        rig.set16(common::QUEUE_NOTIFY_OFF, 2);
        let (_block, queue) = queue_block(8);
        let transport = rig.transport(4, 64);
        let kick = transport.setup_queue(1, &queue).expect("setup");
        assert_eq!(kick.queue, 1);
        assert_eq!(rig.get(common::QUEUE_DESC), 0x2_0000_1000u64 as u32);
        assert_eq!(rig.get(common::QUEUE_DESC + 4), 2);
        assert_eq!(
            rig.get(common::QUEUE_DRIVER),
            (0x2_0000_1000u64 + 128) as u32
        );
        assert_eq!(rig.get(common::QUEUE_ENABLE) as u16, 1);

        transport.notify(kick);
        // notify_off 2 * multiplier 4 = byte 8 of the notify structure.
        let notify = rig.notify.as_ptr() as *const u8;
        // SAFETY: inside the 128-byte notify buffer.
        assert_eq!(unsafe { ptr::read(notify.add(8) as *const u16) }, 1);

        // A device that reports a notify address outside its structure is refused.
        let mut rig = Rig::new();
        rig.set16(common::NUM_QUEUES, 4);
        rig.set16(common::QUEUE_SIZE, 256);
        rig.set16(common::QUEUE_NOTIFY_OFF, 0xFFFF);
        let transport = rig.transport(0x1000, 64);
        assert_eq!(transport.setup_queue(0, &queue), Err(Error::BadQueue));
    }

    #[test]
    fn setup_queue_rejects_missing_or_too_small_queues() {
        let (_block, queue) = queue_block(16);
        let mut rig = Rig::new();
        rig.set16(common::NUM_QUEUES, 2);
        rig.set16(common::QUEUE_SIZE, 8); // device max smaller than ours
        let transport = rig.transport(4, 64);
        assert_eq!(transport.setup_queue(5, &queue), Err(Error::BadQueue));
        assert_eq!(transport.setup_queue(0, &queue), Err(Error::BadQueue));
        rig.set16(common::QUEUE_SIZE, 0); // queue not available
        assert_eq!(transport.setup_queue(0, &queue), Err(Error::BadQueue));
    }

    #[test]
    fn device_config_is_bounds_and_alignment_checked() {
        let mut rig = Rig::new();
        let bytes = rig.device.as_mut_ptr() as *mut u8;
        // SAFETY: inside the 32-byte buffer.
        unsafe { ptr::write(bytes.add(4) as *mut u32, 0xAABB_CCDD) };
        let transport = rig.transport(4, 64);
        assert_eq!(transport.device_config(4, 4), Ok(0xAABB_CCDD));
        assert_eq!(transport.device_config(4, 1), Ok(0xDD));
        assert_eq!(transport.device_config(2, 4), Err(Error::BadRequest));
        assert_eq!(transport.device_config(32, 4), Err(Error::BadRequest));
        assert_eq!(
            transport.device_config(u32::MAX - 1, 2),
            Err(Error::BadRequest)
        );
        assert_eq!(transport.device_config(0, 3), Err(Error::BadRequest));
    }
}
