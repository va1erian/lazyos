//! Host tests: identification, bring-up, the PHY and interrupt registers
//! against the chip model. The rings are in `tests_rings.rs`.

use crate::fake::{tx_config_for, Fake, Memory, MAC};
use crate::phy::{self, mii};
use crate::phy_541::{self, Step};
use crate::regs::*;
use crate::setup::{self, Link, SetupError, XID_8168H};
use crate::watchdog::{TxWatchdog, TIMEOUT_TICKS};

fn chip() -> (Memory, Fake) {
    let memory = Memory::new(4096);
    let fake = Fake::new(&memory);
    (memory, fake)
}

#[test]
fn model_ids() {
    assert_eq!(crate::model(0x10EC, 0x8168), Some("RTL8111H/8168H"));
    assert_eq!(crate::model(0x10EC, 0x8169), None, "RTL8169 is not this");
    assert_eq!(crate::model(0x8086, 0x8168), None);
}

#[test]
fn the_xid_is_gathered_from_two_bit_groups() {
    for xid in [0x541u16, 0x540, 0x449, 0x4C0, 0x000] {
        assert_eq!(setup::xid(tx_config_for(xid)), xid, "{xid:#x}");
    }
    // The documented value for the box: bits 30, 28, 26 and 20.
    assert_eq!(setup::xid(0x5500_0000 | 1 << 20), XID_8168H);
}

#[test]
fn only_the_boxs_revision_is_driven() {
    let (_memory, fake) = chip();
    assert_eq!(setup::identify(&fake), Ok(XID_8168H));
    for xid in [0x000u16, 0x2C1, 0x348, 0x449, 0x540, 0x542, 0x6C0, 0x7C0] {
        fake.set_reg32(TX_CONFIG, tx_config_for(xid));
        assert_eq!(
            setup::identify(&fake),
            Err(SetupError::Unsupported { xid }),
            "{xid:#x}"
        );
    }
    fake.set_reg32(TX_CONFIG, u32::MAX);
    assert_eq!(setup::identify(&fake), Err(SetupError::DeviceGone));
}

#[test]
fn reset_quiets_the_chip() {
    let (_memory, fake) = chip();
    let mut regs = fake.clone();
    regs.write16(INTR_MASK, int::WANTED);
    fake.raise(int::RX_OK | int::LINK_CHANGE);
    setup::reset(&mut regs, || {}).unwrap();
    assert_eq!(fake.reg8(CHIP_CMD) & cmd::RESET, 0);
    assert_eq!(fake.reg16(INTR_MASK), 0);
    assert_eq!(regs.read16(INTR_STATUS), 0);
}

#[test]
fn a_reset_that_never_finishes_times_out() {
    let (_memory, fake) = chip();
    fake.0.borrow_mut().reset_reads = u32::MAX;
    let mut regs = fake.clone();
    let mut naps = 0;
    assert_eq!(
        setup::reset(&mut regs, || naps += 1),
        Err(SetupError::ResetTimeout)
    );
    assert!(naps >= 999, "gave up after {naps} naps");
}

#[test]
fn station_address() {
    let (_memory, fake) = chip();
    assert_eq!(setup::mac(&fake), Ok(MAC));
    // A group address is never a station address, nor is zero.
    fake.set_reg8(IDR0, 0x01);
    assert_eq!(setup::mac(&fake), Err(SetupError::NoMac));
    for byte in 0..6 {
        fake.set_reg8(IDR0 + byte, 0);
    }
    assert_eq!(setup::mac(&fake), Err(SetupError::NoMac));
}

#[test]
fn link_decoding() {
    let (_memory, fake) = chip();
    let cases = [
        (
            phy_status::LINK | phy_status::SPEED_1000 | phy_status::FULL_DUPLEX,
            Link {
                up: true,
                mbps: 1000,
                full_duplex: true,
            },
        ),
        (
            phy_status::LINK | phy_status::SPEED_100,
            Link {
                up: true,
                mbps: 100,
                full_duplex: false,
            },
        ),
        (
            phy_status::LINK | phy_status::SPEED_10 | phy_status::FULL_DUPLEX,
            Link {
                up: true,
                mbps: 10,
                full_duplex: true,
            },
        ),
        // Speed bits without a link say nothing.
        (
            phy_status::SPEED_1000,
            Link {
                up: false,
                mbps: 0,
                full_duplex: false,
            },
        ),
        (
            0,
            Link {
                up: false,
                mbps: 0,
                full_duplex: false,
            },
        ),
    ];
    for (status, want) in cases {
        fake.set_reg8(PHY_STATUS, status);
        assert_eq!(setup::link(&fake), Some(want), "{status:#04x}");
    }
    fake.set_reg8(PHY_STATUS, 0xFF);
    assert_eq!(setup::link(&fake), None, "all ones is a gone device");
}

#[test]
fn interrupt_causes_are_cleared_by_writing_them_back() {
    let (_memory, fake) = chip();
    let mut regs = fake.clone();
    setup::enable_interrupts(&mut regs);
    assert_eq!(fake.reg16(INTR_MASK), int::WANTED);
    // The mask write must not have acknowledged anything.
    fake.raise(int::LINK_CHANGE | int::RX_OK);
    assert_eq!(setup::take_causes(&mut regs), int::LINK_CHANGE | int::RX_OK);
    assert_eq!(setup::take_causes(&mut regs), 0);
    // A cause that arrives between the read and the clear survives: only the
    // bits read are written back.
    fake.raise(int::TX_OK);
    let seen = regs.read16(INTR_STATUS);
    fake.raise(int::RX_OK);
    regs.write16(INTR_STATUS, seen);
    assert_eq!(regs.read16(INTR_STATUS), int::RX_OK);
    fake.0.borrow_mut().causes = u16::MAX;
    assert_eq!(setup::take_causes(&mut regs), u16::MAX);
    assert_eq!(
        regs.read16(INTR_STATUS),
        u16::MAX,
        "a gone device is not acked"
    );
}

#[test]
fn shutdown_stops_the_chip() {
    let (_memory, fake) = chip();
    let mut regs = fake.clone();
    regs.write8(CHIP_CMD, cmd::RX_ENABLE | cmd::TX_ENABLE);
    setup::enable_interrupts(&mut regs);
    setup::shutdown(&mut regs, || {}).unwrap();
    assert_eq!(fake.reg8(CHIP_CMD), 0);
    assert_eq!(fake.reg16(INTR_MASK), 0);
}

#[test]
fn phy_reads_and_writes_through_phyar() {
    let (_memory, fake) = chip();
    let mut regs = fake.clone();
    let mut naps = 0;
    assert_eq!(phy::read(&mut regs, mii::PHYID2, || naps += 1), Ok(0xC800));
    assert!(naps > 0, "waited for the access to complete");
    phy::write(&mut regs, mii::ANAR, 0x1234, || {}).unwrap();
    assert_eq!(fake.0.borrow().phy[4], 0x1234);
    phy::update(&mut regs, mii::ANAR, 0x00FF, 0xFFFF, || {}).unwrap();
    assert_eq!(fake.0.borrow().phy[4], 0x12FF);
}

#[test]
fn phy_accesses_that_never_complete_time_out() {
    let (_memory, fake) = chip();
    fake.0.borrow_mut().phy_busy_reads = u32::MAX;
    let mut regs = fake.clone();
    assert_eq!(phy::read(&mut regs, 1, || {}), Err(SetupError::PhyTimeout));
    assert_eq!(
        phy::write(&mut regs, 0, 0, || {}),
        Err(SetupError::PhyTimeout)
    );
}

#[test]
fn autonegotiation_advertises_everything_and_restarts() {
    let (_memory, fake) = chip();
    fake.0.borrow_mut().phy[9] = 0x0100 | 0x1C00; // 1000 half set, other bits
    let mut regs = fake.clone();
    let id = phy::start_autoneg(&mut regs, || {}).unwrap();
    assert_eq!(id, 0x001C_C800);
    let phy = fake.0.borrow().phy;
    assert_eq!(phy[31], 0, "standard page");
    assert_eq!(phy[4], 0x01E1, "10/100, half and full, no pause");
    assert_eq!(phy[9] & 0x0300, 0x0200, "1000 full only");
    assert_eq!(phy[9] & 0xFC00, 0x1C00, "other GBCR bits untouched");
    assert_eq!(phy[0], mii::BMCR_AUTONEG | mii::BMCR_RESTART);
}

#[test]
fn a_missing_phy_is_named() {
    let (_memory, fake) = chip();
    fake.0.borrow_mut().phy[2] = 0xFFFF;
    fake.0.borrow_mut().phy[3] = 0xFFFF;
    let mut regs = fake.clone();
    assert_eq!(phy::id(&mut regs, || {}), Err(SetupError::NoPhy));
}

#[test]
fn revision_steps_are_read_modify_write_and_none_are_needed_yet() {
    assert!(
        phy_541::STEPS.is_empty(),
        "add a step only with its symptom"
    );
    let (_memory, fake) = chip();
    fake.0.borrow_mut().phy[17] = 0xABCD;
    let mut regs = fake.clone();
    phy_541::apply(&mut regs, || {}).unwrap();
    let steps = [Step {
        reg: 17,
        mask: 0x00F0,
        value: 0x0000,
    }];
    phy_541::apply_steps(&mut regs, &steps, || {}).unwrap();
    assert_eq!(fake.0.borrow().phy[17], 0xAB0D);
}

#[test]
fn the_dump_reads_the_first_256_bytes() {
    let (_memory, fake) = chip();
    let mut out = [0u8; DUMP_BYTES];
    setup::dump(&fake, &mut out);
    assert_eq!(&out[..6], &MAC);
    assert_eq!(
        u32::from_le_bytes(out[0x40..0x44].try_into().unwrap()),
        tx_config_for(XID_8168H)
    );
}

#[test]
fn the_watchdog_fires_only_on_a_stuck_queue_with_link() {
    let mut dog = TxWatchdog::new(0);
    // Idle, or progressing, never fires however long it lasts.
    assert!(dog.check(10_000, 0, 0, true).is_ok());
    for step in 1..=20u64 {
        assert!(dog.check(10_000 + step * 400, 3, step, true).is_ok());
    }
    // Stuck: queued, no completions.
    let start = 100_000;
    assert!(dog.check(start, 2, 50, true).is_ok());
    assert!(dog.check(start + TIMEOUT_TICKS - 1, 2, 50, true).is_ok());
    assert!(dog.check(start + TIMEOUT_TICKS, 2, 50, true).is_err());
    // No link: nothing to blame the chip for, and the clock restarts.
    let mut dog = TxWatchdog::new(0);
    assert!(dog.check(TIMEOUT_TICKS * 3, 2, 0, false).is_ok());
    assert!(dog.check(TIMEOUT_TICKS * 3 + 1, 2, 0, true).is_ok());
    // One completion restarts the clock.
    let mut dog = TxWatchdog::new(0);
    assert!(dog.check(TIMEOUT_TICKS - 1, 2, 0, true).is_ok());
    assert!(dog.check(TIMEOUT_TICKS, 2, 1, true).is_ok());
    assert!(dog.check(TIMEOUT_TICKS * 2 - 1, 2, 1, true).is_ok());
}
