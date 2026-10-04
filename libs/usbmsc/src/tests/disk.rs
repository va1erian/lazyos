//! The disk layer against the model: bring-up, sense-driven retries, block
//! I/O across the transfer limit, write protect and the cache flush.

use std::vec;
use std::vec::Vec;

use super::model::{Model, BLOCK};
use crate::disk::{Disk, DiskError, MAX_TRANSFER, READY_ATTEMPTS};
use crate::scsi::op;

fn up(model: &mut Model) -> Disk {
    Disk::bring_up(model, 0).expect("bring-up")
}

#[test]
fn bring_up_reads_identity_and_geometry() {
    let mut model = Model::new(2048);
    let disk = up(&mut model);
    assert_eq!(disk.capacity.blocks, 2048);
    assert_eq!(disk.block_len(), 512);
    assert!(!disk.write_protected);
    assert!(disk.inquiry.removable);
    assert_eq!(&disk.inquiry.vendor, b"LAZYOS  ");
    assert_eq!(
        model.opcodes,
        [
            op::INQUIRY,
            op::TEST_UNIT_READY,
            op::READ_CAPACITY_10,
            op::MODE_SENSE_6
        ]
    );
}

#[test]
fn unit_attention_and_not_ready_are_retried() {
    let mut model = Model::new(64);
    model.faults.unit_attention = 1;
    model.faults.not_ready = 3;
    let disk = up(&mut model);
    assert_eq!(disk.capacity.blocks, 64);
    assert_eq!(model.delays, 3, "one back-off per NOT READY");
}

#[test]
fn a_unit_that_never_becomes_ready_gives_up() {
    let mut model = Model::new(64);
    model.faults.not_ready = u32::MAX;
    assert_eq!(
        Disk::bring_up(&mut model, 0).err(),
        Some(DiskError::NoMedium)
    );
    assert_eq!(model.delays, READY_ATTEMPTS);
}

#[test]
fn no_medium_is_final() {
    let mut model = Model::new(64);
    model.faults.no_medium = true;
    assert_eq!(
        Disk::bring_up(&mut model, 0).err(),
        Some(DiskError::NoMedium)
    );
}

#[test]
fn large_media_use_read_capacity_16_and_read_16() {
    let blocks = (1u64 << 32) + 100;
    let mut model = Model::new(blocks);
    let mut disk = up(&mut model);
    assert_eq!(disk.capacity.blocks, blocks);
    let lba = (1u64 << 32) + 5;
    let data = vec![0xC3u8; 2 * BLOCK];
    disk.write(&mut model, lba, &data).unwrap();
    let mut back = vec![0u8; 2 * BLOCK];
    disk.read(&mut model, lba, &mut back).unwrap();
    assert_eq!(back, data);
    assert!(model.opcodes.contains(&op::SERVICE_ACTION_IN_16));
    assert!(model.opcodes.contains(&op::WRITE_16));
    assert!(model.opcodes.contains(&op::READ_16));
}

#[test]
fn transfers_split_at_the_limit_and_round_trip() {
    let mut model = Model::new(4096);
    let mut disk = up(&mut model);
    let len = 3 * MAX_TRANSFER + 5 * BLOCK;
    let data: Vec<u8> = (0..len).map(|i| (i * 7 + i / 512) as u8).collect();
    disk.write(&mut model, 17, &data).unwrap();
    let writes = model.opcodes.iter().filter(|&&o| o == op::WRITE_10).count();
    assert_eq!(writes, 4);
    let mut back = vec![0u8; len];
    disk.read(&mut model, 17, &mut back).unwrap();
    assert_eq!(back, data);
    assert_eq!(model.block(16), vec![0; BLOCK], "nothing written before");
}

#[test]
fn ranges_outside_the_medium_never_reach_the_wire() {
    let mut model = Model::new(100);
    let mut disk = up(&mut model);
    let before = model.opcodes.len();
    let mut buf = vec![0u8; 2 * BLOCK];
    assert_eq!(disk.read(&mut model, 99, &mut buf), Err(DiskError::Range));
    assert_eq!(
        disk.read(&mut model, u64::MAX, &mut buf),
        Err(DiskError::Range)
    );
    assert_eq!(
        disk.read(&mut model, 0, &mut buf[..100]),
        Err(DiskError::Range)
    );
    assert_eq!(disk.write(&mut model, 0, &[]), Err(DiskError::Range));
    assert_eq!(model.opcodes.len(), before);
}

#[test]
fn write_protect_is_seen_and_enforced() {
    let mut model = Model::new(100);
    model.faults.write_protect = true;
    let mut disk = up(&mut model);
    assert!(disk.write_protected);
    assert_eq!(
        disk.write(&mut model, 0, &[1u8; BLOCK]),
        Err(DiskError::WriteProtected)
    );
    // A device that hides its write protect is caught by the sense data.
    disk.write_protected = false;
    assert_eq!(
        disk.write(&mut model, 0, &[1u8; BLOCK]),
        Err(DiskError::WriteProtected)
    );
}

#[test]
fn a_refused_mode_sense_means_writable() {
    let mut model = Model::new(100);
    model.faults.refuse_mode_sense = true;
    let disk = up(&mut model);
    assert!(!disk.write_protected);
}

#[test]
fn transient_failures_are_retried_and_persistent_ones_reported() {
    let mut model = Model::new(100);
    let mut disk = up(&mut model);
    model.data.insert(4, vec![9; BLOCK]);
    let mut buf = vec![0u8; BLOCK];
    model.faults.stall_csw = 2; // a reset, then the retry passes
    disk.read(&mut model, 4, &mut buf).unwrap();
    assert_eq!(buf, vec![9; BLOCK]);
    model.faults.short_read = 1; // a short transfer is an error, not data
    assert_eq!(disk.read(&mut model, 4, &mut buf), Err(DiskError::Io));
    model.faults.fail_transfer = u32::MAX; // MEDIUM ERROR every time
    assert_eq!(disk.read(&mut model, 4, &mut buf), Err(DiskError::Io));
}

#[test]
fn flush_issues_synchronize_cache_and_tolerates_its_absence() {
    let mut model = Model::new(100);
    let mut disk = up(&mut model);
    disk.flush(&mut model).unwrap();
    assert_eq!(model.syncs, 1);
    model.faults.unsupported_sync = true;
    disk.flush(&mut model).unwrap();
}

#[test]
fn a_vanished_device_fails_fast() {
    let mut model = Model::new(100);
    let mut disk = up(&mut model);
    model.faults.gone = true;
    let mut buf = vec![0u8; BLOCK];
    assert_eq!(disk.read(&mut model, 0, &mut buf), Err(DiskError::Gone));
    assert_eq!(disk.flush(&mut model), Err(DiskError::Gone));
}
