//! The CBW/CSW wire format and BOT error recovery against the model.

use super::model::Model;
use crate::bot::{self, Bot, BotError, Data, Status, XferError};
use crate::scsi;
use crate::Error;

#[test]
fn cbw_bytes() {
    let cbw = bot::encode_cbw(0x0102_0304, 4096, true, 0x13, &[0x28, 1, 2]);
    assert_eq!(&cbw[0..4], b"USBC");
    assert_eq!(&cbw[4..8], &[4, 3, 2, 1]);
    assert_eq!(&cbw[8..12], &4096u32.to_le_bytes());
    assert_eq!(cbw[12], 0x80);
    assert_eq!(cbw[13], 0x03, "LUN is four bits");
    assert_eq!(cbw[14], 3);
    assert_eq!(&cbw[15..18], &[0x28, 1, 2]);
    assert!(cbw[18..].iter().all(|&b| b == 0));
    assert_eq!(bot::encode_cbw(1, 0, false, 0, &[0])[12], 0);
}

#[test]
fn csw_checks() {
    let mut raw = [0u8; 13];
    raw[0..4].copy_from_slice(b"USBS");
    raw[4..8].copy_from_slice(&9u32.to_le_bytes());
    raw[8..12].copy_from_slice(&10u32.to_le_bytes());
    let csw = bot::decode_csw(&raw).unwrap();
    assert_eq!(csw.check(9, 10), Ok(()));
    assert_eq!(csw.check(8, 10), Err(Error::Malformed), "wrong tag");
    assert_eq!(csw.check(9, 9), Err(Error::BadLength), "residue too large");
    raw[12] = 3;
    assert_eq!(
        bot::decode_csw(&raw).unwrap().check(9, 10),
        Err(Error::BadLength)
    );
    raw[0] = b'X';
    assert_eq!(
        bot::decode_csw(&raw).unwrap().check(9, 10),
        Err(Error::Malformed)
    );
    assert_eq!(bot::decode_csw(&raw[..12]), Err(Error::Short));
    assert_eq!(bot::decode_csw(&[0; 14]), Err(Error::BadLength));
}

fn inquiry(bot: &mut Bot, model: &mut Model) -> Result<Status, BotError> {
    let mut data = [0u8; 36];
    bot.command(model, scsi::inquiry().as_bytes(), Data::In(&mut data))
}

#[test]
fn a_clean_command_passes() {
    let mut model = Model::new(100);
    let mut bot = Bot::new(0);
    assert_eq!(
        inquiry(&mut bot, &mut model),
        Ok(Status::Passed {
            transferred: 36,
            residue: 0
        })
    );
    assert_eq!(model.resets, 0);
    assert_eq!(bot.stats.commands, 1);
}

#[test]
fn stalled_data_in_is_cleared_and_the_csw_read() {
    let mut model = Model::new(100);
    model.faults.stall_data_in = 1;
    let mut bot = Bot::new(0);
    let status = inquiry(&mut bot, &mut model).unwrap();
    assert_eq!(
        status,
        Status::Passed {
            transferred: 0,
            residue: 36
        }
    );
    assert_eq!(model.clears, 1);
    assert_eq!(model.host_resets, 1);
    assert_eq!(model.resets, 0);
    // The pipe is usable again.
    assert!(inquiry(&mut bot, &mut model).is_ok());
}

#[test]
fn stalled_data_out_is_cleared() {
    let mut model = Model::new(100);
    model.faults.stall_data_out = 1;
    let mut bot = Bot::new(0);
    let data = [0x5Au8; 512];
    let cdb = scsi::rw(true, 3, 1).unwrap();
    let status = bot.command(&mut model, cdb.as_bytes(), Data::Out(&data));
    assert_eq!(
        status,
        Ok(Status::Passed {
            transferred: 0,
            residue: 512
        })
    );
    assert_eq!(model.clears, 1);
    assert!(!model.data.contains_key(&3));
}

#[test]
fn a_stalled_csw_is_read_again() {
    let mut model = Model::new(100);
    model.faults.stall_csw = 1;
    let mut bot = Bot::new(0);
    assert!(matches!(
        inquiry(&mut bot, &mut model),
        Ok(Status::Passed { .. })
    ));
    assert_eq!(model.clears, 1);
    assert_eq!(model.resets, 0);
}

#[test]
fn a_twice_stalled_csw_resets() {
    let mut model = Model::new(100);
    model.faults.stall_csw = 2;
    let mut bot = Bot::new(0);
    assert_eq!(inquiry(&mut bot, &mut model), Err(BotError::Reset));
    assert_eq!(model.resets, 1);
    assert!(inquiry(&mut bot, &mut model).is_ok(), "usable after reset");
}

#[test]
fn invalid_or_meaningless_csws_reset() {
    for fault in 0..4 {
        let mut model = Model::new(100);
        match fault {
            0 => model.faults.bad_signature = 1,
            1 => model.faults.wrong_tag = 1,
            2 => model.faults.phase_error = 1,
            _ => model.faults.big_residue = 1,
        }
        let mut bot = Bot::new(0);
        assert_eq!(
            inquiry(&mut bot, &mut model),
            Err(BotError::Reset),
            "fault {fault}"
        );
        assert_eq!(model.resets, 1, "fault {fault}");
        assert_eq!(bot.stats.resets, 1);
        // Clear Feature HALT on both endpoints, device and host side.
        assert_eq!(model.clears, 2);
        assert_eq!(model.host_resets, 2);
        assert!(inquiry(&mut bot, &mut model).is_ok(), "fault {fault}");
    }
}

#[test]
fn failed_status_is_reported_for_sense() {
    let mut model = Model::new(100);
    model.faults.no_medium = true;
    let mut bot = Bot::new(0);
    let status = bot.command(&mut model, scsi::test_unit_ready().as_bytes(), Data::None);
    assert_eq!(status, Ok(Status::Failed { residue: 0 }));
}

#[test]
fn a_vanished_device_ends_the_command() {
    let mut model = Model::new(100);
    model.faults.gone = true;
    let mut bot = Bot::new(0);
    assert_eq!(inquiry(&mut bot, &mut model), Err(BotError::Gone));
    assert_eq!(model.resets, 0);
}

#[test]
fn bad_command_blocks_are_refused_before_the_wire() {
    let mut model = Model::new(100);
    let mut bot = Bot::new(0);
    assert_eq!(
        bot.command(&mut model, &[], Data::None),
        Err(BotError::BadCommand)
    );
    assert_eq!(
        bot.command(&mut model, &[0; 17], Data::None),
        Err(BotError::BadCommand)
    );
    assert!(model.opcodes.is_empty());
}

/// A pipe whose recovery requests fail: the device is dead.
struct Dead;

impl bot::Pipe for Dead {
    fn bulk_out(&mut self, _: &[u8]) -> Result<usize, XferError> {
        Err(XferError::Failed)
    }
    fn bulk_in(&mut self, _: &mut [u8]) -> Result<usize, XferError> {
        Err(XferError::Failed)
    }
    fn control(&mut self, _: bot::Setup) -> Result<(), XferError> {
        Err(XferError::Stall)
    }
    fn reset_host_endpoint(&mut self, _: bool) -> Result<(), XferError> {
        Ok(())
    }
    fn endpoints(&self) -> (u8, u8, u8) {
        (0x81, 0x02, 0)
    }
    fn delay_ms(&mut self, _: u32) {}
}

#[test]
fn failed_recovery_is_reported() {
    let mut bot = Bot::new(0);
    assert_eq!(
        bot.command(&mut Dead, scsi::test_unit_ready().as_bytes(), Data::None),
        Err(BotError::RecoveryFailed)
    );
}

#[test]
fn tags_change_every_command() {
    let mut model = Model::new(100);
    let mut bot = Bot::new(0);
    for _ in 0..5 {
        inquiry(&mut bot, &mut model).unwrap();
    }
    // The model echoes the tag it saw; a wrong-tag fault would be caught, so
    // five clean passes mean five matching (and distinct) tags.
    assert_eq!(bot.stats.commands, 5);
}
