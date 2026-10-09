//! Reads, writes, flushes, errors, timeouts and power-off.

use std::vec::Vec;

use super::model::{Kind, Model};
use super::*;
use crate::regs::{self, cmd, px};
use crate::{Error, Op};

const SECTORS: u64 = 8192;

fn write(
    model: &Model,
    port: &mut Port,
    lba: u64,
    data: &[u8],
    offset: u64,
    first: u64,
) -> Result<(), Error> {
    let buffer = Buffer::new(0x4000_0000, offset, data.len(), first);
    buffer.fill(model, data);
    let mut wait = waiter(model, 100_000);
    port.transfer(
        model,
        Op::Write,
        lba,
        &[(buffer.virt, data.len())],
        &|virt| buffer.translate(virt),
        &mut *wait,
    )
}

fn read(
    model: &Model,
    port: &mut Port,
    lba: u64,
    len: usize,
    offset: u64,
    first: u64,
) -> Result<Vec<u8>, Error> {
    let buffer = Buffer::new(0x5000_0000, offset, len, first);
    let mut wait = waiter(model, 100_000);
    port.transfer(
        model,
        Op::Read,
        lba,
        &[(buffer.virt, len)],
        &|virt| buffer.translate(virt),
        &mut *wait,
    )?;
    Ok(buffer.contents(model))
}

fn disk_range(model: &Model, lba: u64, len: usize) -> Vec<u8> {
    model.disk_bytes(0)[(lba * 512) as usize..][..len].to_vec()
}

#[test]
fn round_trip_at_every_alignment() {
    let model = Model::new(SECTORS);
    let mut port = open(&model).unwrap();
    for (case, &(lba, sectors, offset)) in [
        (0u64, 1usize, 0u64),
        (1, 1, 2),
        (7, 8, 0),
        (9, 9, 6),
        (100, 64, 0),
        (4000, 300, 4094),
        (SECTORS - 3, 3, 0),
    ]
    .iter()
    .enumerate()
    {
        let data = pattern(sectors * 512, case as u8);
        write(&model, &mut port, lba, &data, offset, 0).unwrap();
        assert_eq!(disk_range(&model, lba, data.len()), data, "write {case}");
        let back = read(&model, &mut port, lba, data.len(), offset, 700).unwrap();
        assert_eq!(back, data, "read {case}");
    }
}

#[test]
fn large_transfer_uses_several_slots() {
    let model = Model::new(SECTORS);
    model.behavior(|behavior| behavior.latency = 3);
    let mut port = open(&model).unwrap();
    let data = pattern(2 << 20, 9);
    write(&model, &mut port, 0, &data, 0, 0).unwrap();
    assert_eq!(disk_range(&model, 0, data.len()), data);
    let (commands, depth) = model.port(0, |port| (port.commands.clone(), port.max_inflight));
    // 2 MiB in 256 KiB commands, each a whole number of sectors.
    assert_eq!(commands.len(), 8);
    assert!(commands.iter().all(|&(_, _, count)| count == 512));
    assert!(depth > 1 && depth <= 8, "overlapped: {depth}");
    let back = read(&model, &mut port, 0, data.len(), 0, 900).unwrap();
    assert_eq!(back, data);
}

#[test]
fn vectored_segments_are_one_transfer() {
    let model = Model::new(SECTORS);
    let mut port = open(&model).unwrap();
    let data = pattern(3 * 512 + 1024, 3);
    let a = Buffer::new(0x4000_0000, 0, 700 + 52, 0);
    let b = Buffer::new(0x4100_0000, 0, data.len() - 752, 10);
    // Segments need not be sector multiples individually: 752 + the rest.
    let split = a.len;
    a.fill(&model, &data[..split]);
    b.fill(&model, &data[split..]);
    let translate = |virt: u64| a.translate(virt).or_else(|| b.translate(virt));
    let mut wait = waiter(&model, 100_000);
    port.transfer(
        &model,
        Op::Write,
        20,
        &[(a.virt, a.len), (b.virt, b.len)],
        &translate,
        &mut *wait,
    )
    .unwrap();
    assert_eq!(disk_range(&model, 20, data.len()), data);
}

#[test]
fn out_of_range_and_partial_sectors_are_refused() {
    let model = Model::new(SECTORS);
    let mut port = open(&model).unwrap();
    assert_eq!(
        read(&model, &mut port, SECTORS - 1, 1024, 0, 0),
        Err(Error::Bounds)
    );
    assert_eq!(
        read(&model, &mut port, SECTORS, 512, 0, 0),
        Err(Error::Bounds)
    );
    assert_eq!(
        read(&model, &mut port, u64::MAX, 512, 0, 0),
        Err(Error::Bounds)
    );
    assert_eq!(read(&model, &mut port, 0, 700, 0, 0), Err(Error::Bounds));
    assert!(read(&model, &mut port, 0, 0, 0, 0).unwrap().is_empty());
    assert!(model.port(0, |port| port.commands.is_empty()));
}

#[test]
fn an_odd_address_is_reported_for_bouncing() {
    let model = Model::new(SECTORS);
    let mut port = open(&model).unwrap();
    assert_eq!(
        read(&model, &mut port, 0, 512, 1, 0),
        Err(Error::Misaligned)
    );
    // The port is still good.
    assert!(read(&model, &mut port, 0, 512, 0, 0).is_ok());
}

#[test]
fn an_unmapped_page_is_reported() {
    let model = Model::new(SECTORS);
    let mut port = open(&model).unwrap();
    let mut wait = waiter(&model, 1000);
    let result = port.transfer(&model, Op::Read, 0, &[(0x1000, 512)], &|_| None, &mut *wait);
    assert_eq!(result, Err(Error::Unmapped));
}

#[test]
fn a_task_file_error_fails_the_transfer_and_the_port_recovers() {
    let model = Model::new(SECTORS);
    model.behavior(|behavior| {
        behavior.fail_lba = Some(700);
        behavior.latency = 2;
        behavior.busy_after_error = true;
    });
    let mut port = open(&model).unwrap();
    let result = read(&model, &mut port, 0, 2 << 20, 0, 0);
    assert!(matches!(result, Err(Error::TaskFile { .. })), "{result:?}");
    assert!(!port.is_detached());
    assert_eq!(
        model.port(0, |port| port.comresets),
        1,
        "BSY after the error"
    );
    // After the error the port serves reads that avoid the bad sector.
    model.behavior(|behavior| behavior.fail_lba = None);
    let data = pattern(8192, 4);
    write(&model, &mut port, 1000, &data, 0, 0).unwrap();
    assert_eq!(read(&model, &mut port, 1000, 8192, 0, 100).unwrap(), data);
}

#[test]
fn an_error_without_busy_needs_no_comreset() {
    let model = Model::new(SECTORS);
    model.behavior(|behavior| behavior.fail_lba = Some(5));
    let mut port = open(&model).unwrap();
    assert!(read(&model, &mut port, 0, 8192, 0, 0).is_err());
    assert_eq!(model.port(0, |port| port.comresets), 0);
    model.behavior(|behavior| behavior.fail_lba = None);
    assert!(read(&model, &mut port, 0, 8192, 0, 0).is_ok());
}

#[test]
fn repeated_bad_sectors_do_not_detach_a_healthy_port() {
    let model = Model::new(SECTORS);
    model.behavior(|behavior| behavior.fail_lba = Some(5));
    let mut port = open(&model).unwrap();
    for _ in 0..5 {
        assert!(read(&model, &mut port, 0, 8192, 0, 0).is_err());
    }
    assert!(!port.is_detached());
}

#[test]
fn a_hung_command_times_out_and_the_port_is_stopped() {
    let model = Model::new(SECTORS);
    let mut port = open(&model).unwrap();
    model.behavior(|behavior| behavior.hang = true);
    let buffer = Buffer::new(0x5000_0000, 0, 4096, 0);
    let mut wait = waiter(&model, 50);
    let result = port.transfer(
        &model,
        Op::Read,
        0,
        &[(buffer.virt, 4096)],
        &|virt| buffer.translate(virt),
        &mut *wait,
    );
    assert_eq!(result, Err(Error::Timeout));
    // Recovery restarted the port, with nothing outstanding.
    assert_eq!(model.read32(regs::port_base(0) + px::CI), 0);
    model.behavior(|behavior| behavior.hang = false);
    assert!(read(&model, &mut port, 0, 4096, 0, 0).is_ok());
}

#[test]
fn a_port_that_cannot_be_stopped_is_dma_unsafe_and_detached_at_once() {
    let model = Model::new(SECTORS);
    let mut port = open(&model).unwrap();
    model.behavior(|behavior| {
        behavior.hang = true;
        behavior.never_stop = true;
    });
    let buffer = Buffer::new(0x5000_0000, 0, 4096, 0);
    let mut wait = waiter(&model, 20);
    let result = port.transfer(
        &model,
        Op::Read,
        0,
        &[(buffer.virt, 4096)],
        &|virt| buffer.translate(virt),
        &mut *wait,
    );
    assert!(result.is_err());
    // CR stayed set: the HBA may still be writing into the buffer, so no
    // second chance and the caller is told.
    assert!(port.is_detached());
    assert!(port.dma_unsafe());
    assert_eq!(read(&model, &mut port, 0, 512, 0, 0), Err(Error::Detached));
    assert_eq!(
        port.flush(&model, &mut *waiter(&model, 10)),
        Err(Error::Detached)
    );
}

#[test]
fn a_failed_recovery_with_the_port_stopped_gets_a_second_chance() {
    let model = Model::new(SECTORS);
    model.behavior(|behavior| {
        behavior.fail_lba = Some(5);
        behavior.busy_after_error = true;
        behavior.dead_link = true;
    });
    let mut port = open(&model).unwrap();
    // The error leaves the device busy and the link will not come back:
    // recovery fails, but the port is stopped, so DMA is safe.
    assert!(read(&model, &mut port, 0, 8192, 0, 0).is_err());
    assert!(!port.is_detached());
    assert!(!port.dma_unsafe());
    assert!(read(&model, &mut port, 0, 8192, 0, 0).is_err());
    assert!(port.is_detached(), "two failed recoveries in a row");
    assert!(!port.dma_unsafe());
}

#[test]
fn a_short_prdbc_is_a_failed_command() {
    let model = Model::new(SECTORS);
    model.behavior(|behavior| behavior.short_prdbc = true);
    // IDENTIFY itself is short: the disk is not trusted.
    assert!(matches!(
        open(&model),
        Err(Skip::Failed(Error::ShortTransfer))
    ));
    model.behavior(|behavior| behavior.short_prdbc = false);
    let mut opened = open(&model).unwrap();
    model.behavior(|behavior| behavior.short_prdbc = true);
    let result = read(&model, &mut opened, 0, 4096, 0, 0);
    assert_eq!(result, Err(Error::ShortTransfer));
}

#[test]
fn flush_and_shutdown() {
    let model = Model::new(SECTORS);
    let mut port = open(&model).unwrap();
    port.flush(&model, &mut *waiter(&model, 1000)).unwrap();
    assert_eq!(model.port(0, |port| port.flushes), 1);
    port.shutdown(&model, &mut *waiter(&model, 1000)).unwrap();
    assert_eq!(model.port(0, |port| (port.flushes, port.standbys)), (2, 1));
    assert!(port.is_detached());
    assert_eq!(
        model.read32(regs::port_base(0) + px::CMD) & (cmd::CR | cmd::FR),
        0
    );
    assert_eq!(read(&model, &mut port, 0, 512, 0, 0), Err(Error::Detached));
}

#[test]
fn no_write_cache_means_no_flush() {
    let model = Model::new(SECTORS);
    model.port(0, |port| port.ident.write_cache = false);
    let mut port = open(&model).unwrap();
    port.flush(&model, &mut *waiter(&model, 1000)).unwrap();
    assert_eq!(model.port(0, |port| port.flushes), 0);
}

#[test]
fn shutdown_flushes_even_when_identify_said_the_cache_was_off() {
    let model = Model::new(SECTORS);
    model.port(0, |port| port.ident.write_cache = false);
    let mut port = open(&model).unwrap();
    port.shutdown(&model, &mut *waiter(&model, 1000)).unwrap();
    assert_eq!(model.port(0, |port| (port.flushes, port.standbys)), (1, 1));
}

#[test]
fn a_hung_flush_times_out() {
    let model = Model::new(SECTORS);
    let mut port = open(&model).unwrap();
    model.behavior(|behavior| behavior.hang = true);
    assert_eq!(
        port.flush(&model, &mut *waiter(&model, 20)),
        Err(Error::Timeout)
    );
}

#[test]
fn slots_are_reused_across_transfers() {
    let model = Model::new(SECTORS);
    let mut port = open(&model).unwrap();
    for round in 0..50u64 {
        let data = pattern(40_000 / 512 * 512, round as u8);
        write(&model, &mut port, round * 100 % 4000, &data, 0, round % 7).unwrap();
        let back = read(&model, &mut port, round * 100 % 4000, data.len(), 2, 300).unwrap();
        assert_eq!(back, data);
    }
    assert_eq!(model.read32(regs::port_base(0) + px::CI), 0);
}

#[test]
fn a_second_port_does_not_disturb_the_first() {
    let model = Model::with_ports(
        vec![
            Model::port_of(Kind::Ata, 1024),
            Model::port_of(Kind::Ata, 1024),
        ],
        behavior(),
    );
    let hba = Hba::init(&model).unwrap();
    let mut a = hba.open_port(&model, 0, model.pages()).unwrap();
    let mut b = hba.open_port(&model, 1, model.pages()).unwrap();
    let data = pattern(4096, 1);
    let buffer = Buffer::new(0x4000_0000, 0, 4096, 0);
    buffer.fill(&model, &data);
    let mut wait = waiter(&model, 1000);
    b.transfer(
        &model,
        Op::Write,
        5,
        &[(buffer.virt, 4096)],
        &|v| buffer.translate(v),
        &mut *wait,
    )
    .unwrap();
    assert_eq!(model.disk_bytes(1)[5 * 512..][..4096], data[..]);
    assert!(model.disk_bytes(0).iter().all(|&byte| byte == 0));
    a.detach(&model);
    assert!(!b.is_detached());
}
