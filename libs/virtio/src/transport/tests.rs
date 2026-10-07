//! Host tests of the transport over plain memory standing in for the BARs.

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
fn setup_queue_at_takes_separate_ring_addresses() {
    let mut rig = Rig::new();
    rig.set16(common::NUM_QUEUES, 1);
    rig.set16(common::QUEUE_SIZE, 256);
    let transport = rig.transport(4, 64);
    transport
        .setup_queue_at(0, 128, 0x1000, 0x1800, 0x3_0000_2000)
        .expect("setup");
    assert_eq!(rig.get(common::QUEUE_SIZE) as u16, 128);
    assert_eq!(rig.get(common::QUEUE_DRIVER), 0x1800);
    assert_eq!(rig.get(common::QUEUE_DEVICE), 0x2000);
    assert_eq!(rig.get(common::QUEUE_DEVICE + 4), 3);
    assert_eq!(
        transport.setup_queue_at(0, 0, 0, 0, 0),
        Err(Error::BadQueue)
    );
}

#[test]
fn queue_max_reports_the_device_limit_per_queue() {
    let mut rig = Rig::new();
    rig.set16(common::NUM_QUEUES, 2);
    rig.set16(common::QUEUE_SIZE, 256);
    let transport = rig.transport(4, 64);
    assert_eq!(transport.queue_max(0), Ok(256));
    assert_eq!(rig.get(common::QUEUE_SELECT) as u16, 0);
    assert_eq!(transport.queue_max(1), Ok(256));
    assert_eq!(rig.get(common::QUEUE_SELECT) as u16, 1);
    assert_eq!(
        transport.queue_max(2),
        Err(Error::BadQueue),
        "no such queue"
    );
    rig.set16(common::QUEUE_SIZE, 0);
    assert_eq!(
        transport.queue_max(0),
        Ok(0),
        "an unavailable queue reads 0"
    );
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

#[test]
fn use_msix_points_config_and_enabled_queues_at_the_entry() {
    let mut rig = Rig::new();
    rig.set16(common::NUM_QUEUES, 1);
    rig.set16(common::QUEUE_ENABLE, 1);
    let transport = rig.transport(4, 64);
    assert_eq!(transport.use_msix(0), Ok(()));
    assert_eq!(rig.get(common::MSIX_CONFIG) as u16, 0);
    assert_eq!(rig.get(common::QUEUE_MSIX_VECTOR) as u16, 0);
    // A queue set up afterwards gets the entry too.
    rig.set16(common::QUEUE_SIZE, 8);
    let (_block, queue) = queue_block(8);
    transport.setup_queue(0, &queue).expect("setup");
    assert_eq!(rig.get(common::QUEUE_MSIX_VECTOR) as u16, 0);
}

#[test]
fn queues_default_to_no_vector() {
    let mut rig = Rig::new();
    rig.set16(common::NUM_QUEUES, 1);
    rig.set16(common::QUEUE_SIZE, 8);
    let transport = rig.transport(4, 64);
    let (_block, queue) = queue_block(8);
    transport.setup_queue(0, &queue).expect("setup");
    assert_eq!(rig.get(common::QUEUE_MSIX_VECTOR) as u16, common::NO_VECTOR);
}
