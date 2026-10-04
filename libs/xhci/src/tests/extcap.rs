//! The extended capability walk, the BIOS-to-OS handoff against a model
//! BIOS, and the Supported Protocol table.

use std::cell::Cell;
use std::vec;
use std::vec::Vec;

use crate::extcap::{self, id, legacy, Handoff, Ports};
use crate::regs::{Mmio, Speed};

/// A BAR as dwords, with a BIOS that lets go `release_after` reads after
/// the OS asked (never, for `None`), and a log of every write.
struct FakeBar {
    dwords: Vec<u32>,
    legsup: usize,
    release_after: Option<u32>,
    reads_since_ask: Cell<u32>,
    writes: Vec<(usize, u32)>,
}

impl FakeBar {
    fn new(len: usize) -> FakeBar {
        FakeBar {
            dwords: vec![0; len / 4],
            legsup: usize::MAX,
            release_after: None,
            reads_since_ask: Cell::new(0),
            writes: Vec::new(),
        }
    }

    fn set(&mut self, at: usize, value: u32) {
        self.dwords[at / 4] = value;
    }

    fn get(&self, at: usize) -> u32 {
        self.dwords[at / 4]
    }
}

impl Mmio for FakeBar {
    fn read32(&self, offset: usize) -> u32 {
        let value = self.dwords[offset / 4];
        if offset == self.legsup && value & legacy::OS_OWNED != 0 {
            let reads = self.reads_since_ask.get() + 1;
            self.reads_since_ask.set(reads);
            if self.release_after.is_some_and(|after| reads > after) {
                return value & !legacy::BIOS_OWNED;
            }
        }
        value
    }

    fn write32(&mut self, offset: usize, value: u32) {
        self.writes.push((offset, value));
        self.dwords[offset / 4] = value;
    }
}

/// A capability header: `id`, next in dwords, and the top half.
fn header(cap: u8, next_dwords: u8, high: u16) -> u32 {
    u32::from(cap) | u32::from(next_dwords) << 8 | u32::from(high) << 16
}

/// Legacy support at 0x500 (BIOS owned, SMIs on), a USB 2 protocol at
/// 0x510 for ports 1..=4, a USB 3 one at 0x530 for ports 5..=6.
fn intel_like() -> FakeBar {
    let mut bar = FakeBar::new(0x1000);
    bar.legsup = 0x500;
    bar.set(0x500, header(id::LEGACY, 4, 0) | legacy::BIOS_OWNED);
    bar.set(0x504, 0xE000_E011);
    bar.set(0x510, header(id::PROTOCOL, 8, 0x0200));
    bar.set(0x514, 0x2042_5355);
    bar.set(0x518, 1 | 4 << 8);
    bar.set(0x530, header(id::PROTOCOL, 0, 0x0300));
    bar.set(0x534, 0x2042_5355);
    // Two PSIs: 5 Gb/s SuperSpeed (ID 4) and 10 Gb/s SuperSpeedPlus (ID 5).
    bar.set(0x538, 5 | 2 << 8 | 2 << 28);
    bar.set(0x540, 4 | 3 << 4 | 5 << 16);
    bar.set(0x544, 5 | 3 << 4 | 1 << 14 | 10 << 16);
    bar
}

#[test]
fn handoff_waits_for_the_bios_then_turns_smis_off() {
    let mut bar = intel_like();
    bar.release_after = Some(3);
    let mut waits = 0;
    let result = extcap::legacy_handoff(&mut bar, Some(0x500), 0x1000, || {
        waits += 1;
        true
    });
    assert_eq!(result, Handoff::Released { polls: 3 });
    assert_eq!(waits, 3);
    // The first write asks for ownership; nothing else is touched first.
    assert_eq!(bar.writes[0].0, 0x500);
    assert_ne!(bar.writes[0].1 & legacy::OS_OWNED, 0);
    let control = bar.get(0x504);
    assert_eq!(
        control & (1 | 1 << 4 | 0x7 << 13),
        0,
        "every SMI enable off"
    );
    assert_eq!(
        control & legacy::SMI_EVENTS,
        legacy::SMI_EVENTS,
        "events cleared (RW1C)"
    );
    assert_eq!(
        bar.writes.len(),
        2,
        "USBLEGSUP then USBLEGCTLSTS, nothing else"
    );
}

#[test]
fn handoff_forces_a_bios_that_never_lets_go() {
    let mut bar = intel_like();
    let mut budget = 100;
    let result = extcap::legacy_handoff(&mut bar, Some(0x500), 0x1000, || {
        budget -= 1;
        budget > 0
    });
    assert_eq!(result, Handoff::Forced);
    let legsup = bar.get(0x500);
    assert_eq!(legsup & legacy::BIOS_OWNED, 0);
    assert_ne!(legsup & legacy::OS_OWNED, 0);
}

#[test]
fn handoff_without_a_bios_or_a_capability() {
    let mut bar = intel_like();
    bar.set(0x500, header(id::LEGACY, 4, 0));
    let result = extcap::legacy_handoff(&mut bar, Some(0x500), 0x1000, || panic!("no wait"));
    assert_eq!(result, Handoff::NotOwned);
    assert_ne!(bar.get(0x500) & legacy::OS_OWNED, 0, "semaphore still set");
    let mut none = FakeBar::new(0x1000);
    none.set(0x500, header(id::PROTOCOL, 0, 0x0200));
    assert_eq!(
        extcap::legacy_handoff(&mut none, Some(0x500), 0x1000, || true),
        Handoff::Absent
    );
    assert!(none.writes.is_empty());
    assert_eq!(
        extcap::legacy_handoff(&mut none, None, 0x1000, || true),
        Handoff::Absent
    );
}

#[test]
fn hostile_capability_lists_end() {
    // Offsets only grow, so a cycle cannot be expressed; a chain of
    // hundreds of entries stands in for one and is cut at MAX_CAPS.
    let mut long = FakeBar::new(0x1000);
    for at in (0x100..0x1000 - 16).step_by(4) {
        long.set(at, header(9, 1, 0));
    }
    assert_eq!(
        extcap::caps(&long, Some(0x100), 0x1000).count(),
        extcap::MAX_CAPS
    );
    let mut past = FakeBar::new(0x1000);
    past.set(0xFF0, header(9, 0xFF, 0));
    assert_eq!(extcap::caps(&past, Some(0xFF0), 0x1000).count(), 1);
    assert_eq!(
        extcap::caps(&past, Some(0xFF2), 0x1000).count(),
        0,
        "misaligned"
    );
    assert_eq!(
        extcap::caps(&past, Some(0x2000), 0x1000).count(),
        0,
        "outside"
    );
}

#[test]
fn supported_protocols_name_each_port() {
    let bar = intel_like();
    let ports = Ports::read(&bar, Some(0x500), 0x1000);
    assert!(ports.known());
    assert_eq!(
        (1..=7).map(|p| ports.major(p)).collect::<Vec<_>>(),
        [Some(2), Some(2), Some(2), Some(2), Some(3), Some(3), None]
    );
    assert_eq!(ports.major(0), None);
    // USB 2 ports use the default speed IDs; USB 3 ports their PSI table.
    assert_eq!(ports.speed(1, 1), Some(Speed::Full));
    assert_eq!(ports.speed(2, 2), Some(Speed::Low));
    assert_eq!(ports.speed(3, 3), Some(Speed::High));
    assert_eq!(ports.speed(5, 4), Some(Speed::Super));
    assert_eq!(ports.speed(6, 5), Some(Speed::SuperPlus));
    // A SuperSpeed ID on a USB 2 port, or a USB 2 one on a USB 3 port, is
    // nonsense.
    assert_eq!(ports.speed(1, 4), None);
    assert_eq!(ports.speed(5, 3), None);
    // An unknown port falls back to the default mapping.
    assert_eq!(ports.speed(7, 3), Some(Speed::High));
    assert_eq!(ports.speed(7, 0), None);
}

#[test]
fn protocol_psi_tables_decide_usb2_speeds() {
    let mut bar = FakeBar::new(0x1000);
    bar.set(0x100, header(id::PROTOCOL, 0, 0x0200));
    bar.set(0x104, 0x2042_5355);
    bar.set(0x108, 1 | 2 << 8 | 3 << 28);
    // Custom IDs: 1 = 12 Mb/s, 2 = 1.5 Mb/s, 3 = 480 Mb/s (in Kb/s units).
    bar.set(0x110, 1 | 2 << 4 | 12 << 16);
    bar.set(0x114, 2 | 1 << 4 | 1500 << 16);
    bar.set(0x118, 3 | 2 << 4 | 480 << 16);
    let ports = Ports::read(&bar, Some(0x100), 0x1000);
    assert_eq!(ports.speed(1, 1), Some(Speed::Full));
    assert_eq!(ports.speed(1, 2), Some(Speed::Low));
    assert_eq!(ports.speed(2, 3), Some(Speed::High));
    // A table that runs past the BAR, a bad name, or major 1 is ignored.
    let mut bad = FakeBar::new(0x1000);
    bad.set(0xFF0, header(id::PROTOCOL, 0, 0x0200));
    bad.set(0xFF4, 0x2042_5355);
    bad.set(0xFF8, 1 | 2 << 8 | 15 << 28);
    assert!(!Ports::read(&bad, Some(0xFF0), 0x1000).known());
    let mut named = FakeBar::new(0x1000);
    named.set(0x100, header(id::PROTOCOL, 0, 0x0200));
    named.set(0x104, 0x4242_4242);
    named.set(0x108, 1 | 2 << 8);
    assert!(!Ports::read(&named, Some(0x100), 0x1000).known());
}
