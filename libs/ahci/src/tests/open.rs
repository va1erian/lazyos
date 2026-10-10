//! HBA handoff and port bring-up.

use std::vec;

use super::model::{Ident, Kind, Model};
use super::*;
use crate::identify::Refusal;
use crate::regs::{self, ghc};
use crate::Error;

#[test]
fn hba_init_sets_ae_and_leaves_interrupts_off() {
    let model = Model::new(2048);
    model.write32(regs::GHC, ghc::IE);
    let hba = Hba::init(&model).unwrap();
    let control = model.read32(regs::GHC);
    assert_ne!(control & ghc::AE, 0);
    assert_eq!(control & ghc::IE, 0);
    assert_eq!(hba.implemented, 1);
    assert_eq!(hba.highest_port(), Some(0));
    assert!(hba.has_port(0) && !hba.has_port(1));
    assert!(!hba.cap.s64a);
}

#[test]
fn dead_bus_is_refused() {
    struct Dead;
    impl Platform for Dead {
        fn read32(&self, _: usize) -> u32 {
            u32::MAX
        }
        fn write32(&self, _: usize, _: u32) {}
        fn read_mem(&self, _: u64, _: &mut [u8]) {}
        fn write_mem(&self, _: u64, _: &[u8]) {}
        fn now_ns(&self) -> u64 {
            0
        }
    }
    assert_eq!(Hba::init(&Dead).unwrap_err(), Error::Fatal);
}

#[test]
fn handoff_waits_for_the_bios() {
    let model = Model::new(2048).with(Behavior {
        handoff: true,
        bios_polls: 20,
        bios_busy_polls: 10,
        ..behavior()
    });
    Hba::init(&model).unwrap();
    // The OS owns the HBA: BOS clear, OOS set.
    let bohc = model.read32(regs::BOHC);
    assert_eq!(bohc & regs::bohc::BOS, 0);
    assert_ne!(bohc & regs::bohc::OOS, 0);
}

#[test]
fn a_bios_that_never_lets_go_does_not_hang_init() {
    let model = Model::new(2048).with(Behavior {
        handoff: true,
        bios_polls: u32::MAX,
        ..behavior()
    });
    assert!(Hba::init(&model).is_ok());
    assert!(model.now_ns() < 3_000_000_000, "bounded by 25 ms");
}

#[test]
fn no_handoff_register_means_no_handoff() {
    let model = Model::new(2048);
    Hba::init(&model).unwrap();
    assert_eq!(model.read32(regs::BOHC) & regs::bohc::OOS, 0);
}

#[test]
fn opens_a_disk_and_reads_identify() {
    let model = Model::new(2048);
    let port = open(&model).unwrap();
    assert_eq!(port.disk.sectors, 2048);
    assert_eq!(port.disk.model.as_str(), "Model SSD 512GB");
    assert_eq!(port.disk.serial.as_str(), "SERIAL42");
    assert_eq!(port.disk.firmware.as_str(), "FW1.0");
    assert!(port.disk.write_cache);
    assert!(!port.is_detached());
}

#[test]
fn an_empty_port_costs_almost_nothing() {
    let model = Model::with_ports(vec![Model::port_of(Kind::Empty, 0)], behavior());
    assert_eq!(open(&model).err(), Some(Skip::Empty));
    assert!(
        model.now_ns() < 10_000_000,
        "under 10 ms: {}",
        model.now_ns()
    );
}

#[test]
fn atapi_and_unknown_signatures_are_skipped() {
    let model = Model::with_ports(vec![Model::port_of(Kind::Atapi, 1)], behavior());
    assert_eq!(open(&model).err(), Some(Skip::Atapi));
    let model = Model::with_ports(
        vec![Model::port_of(Kind::Unknown(0x9669_0101), 1)],
        behavior(),
    );
    assert_eq!(open(&model).err(), Some(Skip::Unknown(0x9669_0101)));
}

#[test]
fn a_skipped_port_is_stopped() {
    let model = Model::with_ports(vec![Model::port_of(Kind::Atapi, 1)], behavior());
    open(&model).err();
    assert_eq!(
        model.read32(regs::port_base(0) + regs::px::CMD) & (regs::cmd::ST | regs::cmd::CR),
        0
    );
}

#[test]
fn a_slow_link_and_a_busy_device_are_waited_for() {
    let model = Model::new(2048).with(Behavior {
        settle_polls: 30,
        busy_polls: 200,
        ..behavior()
    });
    assert!(open(&model).is_ok());
}

#[test]
fn identify_variants_are_refused_by_name() {
    type Change = fn(&mut Ident);
    let cases: [(Change, Refusal); 5] = [
        (|i| i.lba48 = false, Refusal::NoLba48),
        (|i| i.flush_ext = false, Refusal::NoFlush),
        (|i| i.sector_bytes = 4096, Refusal::SectorSize(4096)),
        (|i| i.zero_capacity = true, Refusal::BadCapacity),
        (|i| i.garbage = true, Refusal::NotAta),
    ];
    for (change, expected) in cases {
        let model = Model::new(2048);
        model.port(0, |port| change(&mut port.ident));
        let result = open(&model);
        match (expected, result) {
            (Refusal::NotAta, Err(Skip::Refused(_))) => {}
            (expected, Err(Skip::Refused(got))) => assert_eq!(got, expected),
            (expected, other) => panic!("{expected:?}: {:?}", other.err()),
        }
        // A refused disk's port is left stopped.
        assert_eq!(
            model.read32(regs::port_base(0) + regs::px::CMD) & regs::cmd::CR,
            0
        );
    }
}

#[test]
fn a_512e_disk_is_served() {
    let model = Model::new(2048);
    model.port(0, |port| port.ident.physical_shift = 3);
    let port = open(&model).unwrap();
    assert_eq!(port.disk.physical_bytes, 4096);
}

#[test]
fn a_port_that_will_not_stop_is_reset_or_skipped() {
    let model = Model::new(2048).with(Behavior {
        never_stop: true,
        ..behavior()
    });
    // Start the port by hand so CR is set when the driver arrives.
    model.write32(regs::port_base(0) + regs::px::CLB, 0x1000);
    model.write32(regs::port_base(0) + regs::px::FB, 0x2000);
    model.write32(
        regs::port_base(0) + regs::px::CMD,
        regs::cmd::FRE | regs::cmd::ST,
    );
    let result = open(&model);
    assert!(matches!(
        result,
        Err(Skip::Failed(Error::Fatal | Error::Timeout))
    ));
    assert!(model.port(0, |port| port.comresets) >= 1);
}

#[test]
fn ports_beyond_the_count_in_cap_are_ignored() {
    // PI claims many ports but CAP.NP allows one.
    struct Greedy(Model);
    impl Platform for Greedy {
        fn read32(&self, offset: usize) -> u32 {
            if offset == regs::PI {
                return 0xFFFF_FF00;
            }
            self.0.read32(offset)
        }
        fn write32(&self, offset: usize, value: u32) {
            self.0.write32(offset, value)
        }
        fn read_mem(&self, phys: u64, buf: &mut [u8]) {
            self.0.read_mem(phys, buf)
        }
        fn write_mem(&self, phys: u64, data: &[u8]) {
            self.0.write_mem(phys, data)
        }
        fn now_ns(&self) -> u64 {
            self.0.now_ns()
        }
    }
    let hba = Hba::init(&Greedy(Model::new(2048))).unwrap();
    assert_eq!(hba.implemented, 0x100);
    assert_eq!(hba.highest_port(), Some(8));
    assert!(!hba.has_port(40));
}

#[test]
fn two_ports_open_independently() {
    let model = Model::with_ports(
        vec![
            Model::port_of(Kind::Ata, 1024),
            Model::port_of(Kind::Atapi, 1),
            Model::port_of(Kind::Ata, 4096),
        ],
        behavior(),
    );
    let hba = Hba::init(&model).unwrap();
    let first = hba.open_port(&model, 0, model.pages()).unwrap();
    assert_eq!(
        hba.open_port(&model, 1, model.pages()).err(),
        Some(Skip::Atapi)
    );
    let third = hba.open_port(&model, 2, model.pages()).unwrap();
    assert_eq!((first.disk.sectors, third.disk.sectors), (1024, 4096));
    assert_eq!(
        hba.open_port(&model, 3, model.pages()).err(),
        Some(Skip::Empty)
    );
}

#[test]
fn dma_memory_above_4g_is_refused_on_a_32_bit_hba() {
    let model = Model::new(2048);
    let hba = Hba::init(&model).unwrap();
    let mut pages = model.pages();
    pages.list += 1 << 32;
    assert!(matches!(
        hba.open_port(&model, 0, pages).err(),
        Some(Skip::Failed(Error::Unsupported(_)))
    ));
    let mut pages = model.pages();
    pages.list += 512;
    assert!(matches!(
        hba.open_port(&model, 0, pages).err(),
        Some(Skip::Failed(Error::Unsupported(_)))
    ));
}
