//! Bring-up and I/O against the model controller: every PRP shape, the
//! queue wrapping, media errors, stray completions, a hung command, a
//! controller that never comes ready, flush and shutdown.

use std::vec::Vec;

use super::model::{Behavior, Model};
use super::{pattern, translate_all, up, wait_for, Buffer};
use crate::{Controller, Error, Op, Platform};

#[test]
fn brings_up_and_identifies() {
    let model = Model::new(2048, Behavior::default());
    let controller = up(&model);
    assert_eq!(controller.namespace.blocks, 2048);
    assert_eq!(controller.namespace.block_bytes, 512);
    assert_eq!(
        controller.info.model.as_str(),
        "LazyOS model NVMe controller"
    );
    assert_eq!(controller.info.serial.as_str(), "lazyos-model");
    assert_eq!(
        controller.max_command_bytes(),
        crate::controller::MAX_COMMAND_BYTES
    );
}

#[test]
fn never_ready_times_out_and_leaves_it_disabled() {
    let model = Model::new(
        64,
        Behavior {
            never_ready: true,
            ..Behavior::default()
        },
    );
    assert_eq!(
        Controller::init(&model, model.pages()).err(),
        Some(Error::Timeout)
    );
    assert_eq!(model.read32(crate::regs::CC) & 1, 0);
}

#[test]
fn fatal_status_on_enable_is_reported() {
    let model = Model::new(
        64,
        Behavior {
            fatal_on_enable: true,
            ..Behavior::default()
        },
    );
    assert_eq!(
        Controller::init(&model, model.pages()).err(),
        Some(Error::Fatal)
    );
}

#[test]
fn a_4k_namespace_initialises_with_its_block_size() {
    // The library serves any power-of-two block size; the kernel refuses
    // everything but 512 bytes (its block layer is 512 everywhere).
    let model = Model::new(
        64,
        Behavior {
            lba_shift: Some(12),
            ..Behavior::default()
        },
    );
    let controller = up(&model);
    assert_eq!(controller.namespace.block_bytes, 4096);
}

#[test]
fn mdts_caps_commands() {
    // MDTS 2 with 4 KiB pages: 16 KiB per command.
    let model = Model::new(
        1024,
        Behavior {
            mdts: 2,
            ..Behavior::default()
        },
    );
    let mut controller = up(&model);
    assert_eq!(controller.max_command_bytes(), 16 * 1024);
    let buffer = Buffer::new(&model, 0x10_0000, 0, 64 * 1024);
    buffer.fill(&model, &pattern(64 * 1024, 3));
    let translate = translate_all(&[&buffer]);
    let segments = [(buffer.virt, buffer.len)];
    controller
        .transfer(
            &model,
            Op::Write,
            0,
            &segments,
            &translate,
            &mut wait_for(&model),
        )
        .unwrap();
    assert_eq!(model.state.borrow().io_commands, 4);
}

#[test]
fn small_mqes_shrinks_the_queues() {
    let model = Model::new(
        1024,
        Behavior {
            mqes: Some(3),
            ..Behavior::default()
        },
    );
    let mut controller = up(&model);
    let buffer = Buffer::new(&model, 0x20_0000, 0, 32 * 1024);
    buffer.fill(&model, &pattern(32 * 1024, 9));
    let translate = translate_all(&[&buffer]);
    // Many commands through a four-entry queue: the phase bit wraps.
    for round in 0..10u64 {
        controller
            .transfer(
                &model,
                Op::Write,
                round * 64,
                &[(buffer.virt, buffer.len)],
                &translate,
                &mut wait_for(&model),
            )
            .unwrap();
    }
}

#[test]
fn write_then_read_round_trips_every_prp_shape() {
    let model = Model::new(
        4096,
        Behavior {
            defer_io: true,
            ..Behavior::default()
        },
    );
    let mut controller = up(&model);
    // (offset into the first page, length): one page, two pages, a list,
    // an unaligned start, and a transfer of several commands.
    let shapes = [
        (0, 512),
        (0, 4096),
        (0, 8192),
        (0, 12288),
        (512, 4096),
        (2048, 61440),
        (4, 512),
        (1024, 300 * 1024),
    ];
    let mut lba = 0u64;
    for (index, &(offset, len)) in shapes.iter().enumerate() {
        let data = pattern(len, index as u8);
        let out = Buffer::new(&model, 0x100_0000 * (index as u64 + 1), offset, len);
        out.fill(&model, &data);
        let back = Buffer::new(
            &model,
            0x100_0000 * (index as u64 + 1) + 0x80_0000,
            offset,
            len,
        );
        let translate = translate_all(&[&out, &back]);
        controller
            .transfer(
                &model,
                Op::Write,
                lba,
                &[(out.virt, len)],
                &translate,
                &mut wait_for(&model),
            )
            .unwrap();
        controller
            .transfer(
                &model,
                Op::Read,
                lba,
                &[(back.virt, len)],
                &translate,
                &mut wait_for(&model),
            )
            .unwrap();
        assert_eq!(back.contents(&model), data, "shape {offset}+{len}");
        let start = lba as usize * 512;
        assert_eq!(&model.state.borrow().disk[start..start + len], &data[..]);
        lba += (len / 512) as u64;
    }
    // The 300 KiB transfer kept several commands in the queue at once.
    assert!(model.state.borrow().max_pending > 1);
}

#[test]
fn vectored_buffers_split_where_prps_break() {
    let model = Model::new(1024, Behavior::default());
    let mut controller = up(&model);
    // Three 4 KiB blocks of a cache, each at its own page: one command. Then
    // a 512-byte buffer at a page's middle: a new command.
    let a = Buffer::new(&model, 0x30_0000, 0, 4096);
    let b = Buffer::new(&model, 0x40_0000, 0, 4096);
    let c = Buffer::new(&model, 0x50_0000, 0, 4096);
    let d = Buffer::new(&model, 0x60_0000, 1536, 512);
    let mut expected = Vec::new();
    for (index, buffer) in [&a, &b, &c, &d].iter().enumerate() {
        let data = pattern(buffer.len, 40 + index as u8);
        buffer.fill(&model, &data);
        expected.extend_from_slice(&data);
    }
    let buffers = [&a, &b, &c, &d];
    let translate = translate_all(&buffers);
    let segments: Vec<(u64, usize)> = buffers
        .iter()
        .map(|buffer| (buffer.virt, buffer.len))
        .collect();
    controller
        .transfer(
            &model,
            Op::Write,
            10,
            &segments,
            &translate,
            &mut wait_for(&model),
        )
        .unwrap();
    assert_eq!(model.state.borrow().io_commands, 2);
    assert_eq!(
        &model.state.borrow().disk[5120..5120 + expected.len()],
        &expected[..]
    );
}

#[test]
fn media_error_fails_the_transfer_but_not_the_controller() {
    let model = Model::new(
        1024,
        Behavior {
            fail_lba: Some(70),
            defer_io: true,
            ..Behavior::default()
        },
    );
    let mut controller = up(&model);
    let buffer = Buffer::new(&model, 0x70_0000, 0, 128 * 1024);
    let translate = translate_all(&[&buffer]);
    let result = controller.transfer(
        &model,
        Op::Read,
        0,
        &[(buffer.virt, buffer.len)],
        &translate,
        &mut wait_for(&model),
    );
    assert_eq!(result, Err(Error::Status { sct: 2, sc: 0x81 }));
    assert!(!controller.is_detached());
    // Every command it submitted was reaped: the next transfer works.
    controller
        .transfer(
            &model,
            Op::Read,
            0,
            &[(buffer.virt, 4096)],
            &translate,
            &mut wait_for(&model),
        )
        .unwrap();
}

#[test]
fn stray_completions_are_ignored() {
    let model = Model::new(
        1024,
        Behavior {
            stray_completions: true,
            ..Behavior::default()
        },
    );
    let mut controller = up(&model);
    let buffer = Buffer::new(&model, 0x80_0000, 0, 16 * 1024);
    buffer.fill(&model, &pattern(16 * 1024, 77));
    let translate = translate_all(&[&buffer]);
    for lba in 0..20 {
        controller
            .transfer(
                &model,
                Op::Write,
                lba * 32,
                &[(buffer.virt, buffer.len)],
                &translate,
                &mut wait_for(&model),
            )
            .unwrap();
    }
}

#[test]
fn a_hung_command_detaches_the_controller() {
    let model = Model::new(
        256,
        Behavior {
            hang_io: true,
            ..Behavior::default()
        },
    );
    let mut controller = up(&model);
    let buffer = Buffer::new(&model, 0x90_0000, 0, 4096);
    let translate = translate_all(&[&buffer]);
    let result = controller.transfer(
        &model,
        Op::Read,
        0,
        &[(buffer.virt, 4096)],
        &translate,
        &mut wait_for(&model),
    );
    assert_eq!(result, Err(Error::Timeout));
    assert!(controller.is_detached());
    assert_eq!(
        model.read32(crate::regs::CC) & 1,
        0,
        "left enabled after a timeout"
    );
    let again = controller.transfer(
        &model,
        Op::Read,
        0,
        &[(buffer.virt, 4096)],
        &translate,
        &mut wait_for(&model),
    );
    assert_eq!(again, Err(Error::Detached));
}

#[test]
fn bounds_and_partial_blocks_are_refused() {
    let model = Model::new(16, Behavior::default());
    let mut controller = up(&model);
    let buffer = Buffer::new(&model, 0xA0_0000, 0, 8192);
    let translate = translate_all(&[&buffer]);
    let mut wait = wait_for(&model);
    assert_eq!(
        controller.transfer(
            &model,
            Op::Read,
            12,
            &[(buffer.virt, 4096)],
            &translate,
            &mut wait
        ),
        Err(Error::Bounds)
    );
    assert_eq!(
        controller.transfer(
            &model,
            Op::Read,
            0,
            &[(buffer.virt, 100)],
            &translate,
            &mut wait
        ),
        Err(Error::Bounds)
    );
    assert_eq!(
        controller.transfer(
            &model,
            Op::Read,
            u64::MAX,
            &[(buffer.virt, 512)],
            &translate,
            &mut wait
        ),
        Err(Error::Bounds)
    );
    assert_eq!(
        controller.transfer(&model, Op::Read, 0, &[], &translate, &mut wait),
        Ok(())
    );
    assert_eq!(model.state.borrow().io_commands, 0);
}

#[test]
fn unmapped_and_odd_buffers_are_refused() {
    let model = Model::new(64, Behavior::default());
    let mut controller = up(&model);
    let buffer = Buffer::new(&model, 0xB0_0000, 1, 512);
    let translate = translate_all(&[&buffer]);
    let mut wait = wait_for(&model);
    assert_eq!(
        controller.transfer(
            &model,
            Op::Read,
            0,
            &[(buffer.virt, 512)],
            &translate,
            &mut wait
        ),
        Err(Error::Misaligned)
    );
    assert_eq!(
        controller.transfer(
            &model,
            Op::Read,
            0,
            &[(0xDEAD_0000, 512)],
            &translate,
            &mut wait
        ),
        Err(Error::Unmapped)
    );
}

#[test]
fn flush_only_with_a_volatile_cache() {
    let model = Model::new(64, Behavior::default());
    let mut controller = up(&model);
    controller.flush(&model, &mut wait_for(&model)).unwrap();
    assert_eq!(model.state.borrow().flushes, 0);
    let model = Model::new(
        64,
        Behavior {
            vwc: true,
            ..Behavior::default()
        },
    );
    let mut controller = up(&model);
    controller.flush(&model, &mut wait_for(&model)).unwrap();
    assert_eq!(model.state.borrow().flushes, 1);
}

#[test]
fn shutdown_notifies_and_detaches() {
    let model = Model::new(64, Behavior::default());
    let mut controller = up(&model);
    controller.shutdown(&model).unwrap();
    assert_eq!(model.state.borrow().shutdowns, 1);
    assert!(controller.is_detached());
    assert_eq!(controller.shutdown(&model), Err(Error::Detached));

    let model = Model::new(
        64,
        Behavior {
            shutdown_hangs: true,
            ..Behavior::default()
        },
    );
    let mut controller = up(&model);
    assert_eq!(controller.shutdown(&model), Err(Error::Timeout));
}

#[test]
fn reinit_after_a_previous_driver_left_it_enabled() {
    let model = Model::new(64, Behavior::default());
    let _first = up(&model);
    // Firmware (or a crashed boot) left the controller running.
    let second = Controller::init(&model, model.pages()).expect("second bring-up");
    assert_eq!(second.namespace.blocks, 64);
}
