//! Legacy-device presence probes on a board that has none of them: the ATA
//! primary channel, the i8042 and COM1, each driven through a fake port
//! model so a floating bus (`0xFF`), an empty channel and a wedged device
//! are all proven to end quickly and to say "absent".

use super::*;
use crate::block::ata::{self, Channel};
use crate::input::i8042::{self, Controller, Probe};
use crate::serial::{self, Uart};

/// An ATA channel that answers every status read from `status` and counts
/// the reads.
struct FakeAta<F: FnMut(u32) -> u8> {
    status: F,
    reads: u32,
    identify_issued: bool,
    words: [u16; 256],
    next_word: usize,
    /// Nanoseconds the fake clock advances per `now_ns` call; 0 means the
    /// channel has no clock.
    clock_step: u64,
    clock: u64,
    /// Times a wait offered the kernel an interrupt window (`pace`).
    paces: u32,
}

impl<F: FnMut(u32) -> u8> FakeAta<F> {
    fn new(status: F) -> Self {
        FakeAta {
            status,
            reads: 0,
            identify_issued: false,
            words: [0; 256],
            next_word: 0,
            clock_step: 0,
            clock: 0,
            paces: 0,
        }
    }

    /// Give the channel a clock that ticks `step_ns` on every reading.
    fn with_clock(mut self, step_ns: u64) -> Self {
        self.clock_step = step_ns;
        self
    }
}

impl<F: FnMut(u32) -> u8> Channel for FakeAta<F> {
    fn status(&mut self) -> u8 {
        self.reads += 1;
        (self.status)(self.reads)
    }
    fn delay_400ns(&mut self) {}
    fn select_master(&mut self) {}
    fn issue_identify(&mut self) {
        self.identify_issued = true;
    }
    fn read_data(&mut self) -> u16 {
        let word = self.words[self.next_word % 256];
        self.next_word += 1;
        word
    }
    fn pace(&mut self) {
        self.paces += 1;
    }
    fn now_ns(&mut self) -> Option<u64> {
        if self.clock_step == 0 {
            return None;
        }
        self.clock += self.clock_step;
        Some(self.clock)
    }
}

/// A floating bus (no IDE controller, every PC in AHCI mode) is absent after
/// one status read, before IDENTIFY is even sent; QEMU's empty channel (`0`)
/// likewise. Every wait gives up at `0xFF` at once.
pub fn ata_floating_bus_is_absent_at_once() -> Result<(), String> {
    for (what, value) in [("floating bus", 0xFFu8), ("empty channel", 0x00)] {
        let mut fake = FakeAta::new(|_| value);
        check!(
            ata::identify_on(&mut fake).is_none(),
            "{what}: a drive was found"
        );
        check!(fake.reads == 1, "{what}: {} status reads", fake.reads);
        check!(!fake.identify_issued, "{what}: IDENTIFY was sent");
    }
    // A bus that only floats once driven: absent right after the command.
    let mut late = FakeAta::new(|read| if read == 1 { 0x50 } else { 0xFF });
    check!(
        ata::identify_on(&mut late).is_none(),
        "late float found a drive"
    );
    check!(late.reads == 2, "late float: {} reads", late.reads);
    let mut floating = FakeAta::new(|_| 0xFF);
    check!(
        !ata::wait_not_busy_on(&mut floating),
        "wait_not_busy passed on 0xFF"
    );
    check!(
        !ata::wait_for_data_on(&mut floating),
        "wait_for_data passed on 0xFF"
    );
    check!(
        floating.reads == 2,
        "the waits took {} reads on 0xFF",
        floating.reads
    );
    // ERR and DRQ mean nothing while BSY is set: a stale ERR between
    // sectors must not fail the read, nor a stale DRQ start it early.
    let mut stale = FakeAta::new(|read| match read {
        1 => 0x81,
        2 => 0x88,
        _ => 0x58,
    });
    check!(
        ata::wait_for_data_on(&mut stale),
        "a stale ERR under BSY failed the wait"
    );
    check!(stale.reads == 3, "stale bits: {} reads", stale.reads);
    check!(
        ata::status_means_absent(0xFF) && ata::status_means_absent(0),
        "absence rule"
    );
    check!(
        !ata::status_means_absent(0x50),
        "a ready drive reads absent"
    );
    Ok(())
}

/// A drive that never leaves BSY (or never raises DRQ) costs a bounded
/// number of reads, not a hang; a well-behaved fake drive is identified.
pub fn ata_waits_are_bounded() -> Result<(), String> {
    let mut stuck = FakeAta::new(|read| if read <= 2 { 0x50 } else { 0x80 });
    check!(
        ata::identify_on(&mut stuck).is_none(),
        "a stuck drive identified"
    );
    check!(
        stuck.reads <= 2 + ata::POLL_LIMIT,
        "stuck BSY took {} reads",
        stuck.reads
    );
    let mut no_drq = FakeAta::new(|_| 0x50);
    check!(ata::identify_on(&mut no_drq).is_none(), "no DRQ identified");
    check!(
        no_drq.reads <= 3 + ata::POLL_LIMIT,
        "missing DRQ took {} reads",
        no_drq.reads
    );
    let mut good = FakeAta::new(|_| 0x58);
    good.words[60] = 0x1000;
    good.words[61] = 0x0002;
    check!(
        ata::identify_on(&mut good) == Some(0x2_1000),
        "good drive misread"
    );
    Ok(())
}

/// With a clock the wait ends by time, not by read count: a drive stuck busy
/// is given up on after `POLL_TIMEOUT_NS` whether reads are fast (a million
/// of them fit) or slow (a VM exit each, issue #449: a few thousand).
pub fn ata_waits_end_by_deadline() -> Result<(), String> {
    for step in [10_000u64, 1_000_000] {
        let mut stuck = FakeAta::new(|read| if read <= 2 { 0x50 } else { 0x80 }).with_clock(step);
        check!(
            ata::identify_on(&mut stuck).is_none(),
            "step {step}: a stuck drive identified"
        );
        // One clock reading at the start of the wait, then one per status
        // read in `expired`: the clock advances `step` per read, so the wait
        // ends after exactly `POLL_TIMEOUT_NS / step` busy reads, plus the
        // two absence-check reads `identify_on` makes first.
        let expected = ata::POLL_TIMEOUT_NS / step;
        let reads = u64::from(stuck.reads);
        check!(
            reads >= expected && reads <= expected + 4,
            "step {step}: {reads} reads, expected about {expected}"
        );
    }
    // A drive that answers inside the deadline is not cut short.
    let mut slow = FakeAta::new(|read| if read < 500 { 0x80 } else { 0x58 }).with_clock(1_000_000);
    check!(
        ata::wait_for_data_on(&mut slow),
        "a drive ready after 500 reads was abandoned"
    );
    Ok(())
}

/// Every busy status read of a wait offers the kernel an interrupt window
/// (`Channel::pace`, `irq_window::poll_point` on the real ports), so a drive
/// stuck busy for the whole deadline no longer keeps interrupts off for a
/// second (issue #449: the timer and the compositor starved).
pub fn ata_waits_pace_every_read() -> Result<(), String> {
    let mut stuck = FakeAta::new(|_| 0x80).with_clock(1_000_000);
    check!(!ata::wait_not_busy_on(&mut stuck), "a stuck drive settled");
    check!(
        stuck.paces + 1 >= stuck.reads && stuck.paces <= stuck.reads,
        "{} paces for {} busy reads",
        stuck.paces,
        stuck.reads
    );
    let mut no_drq = FakeAta::new(|_| 0x50).with_clock(1_000_000);
    check!(!ata::wait_for_data_on(&mut no_drq), "DRQ appeared");
    check!(
        no_drq.paces + 1 >= no_drq.reads,
        "{} paces for {} reads waiting for DRQ",
        no_drq.paces,
        no_drq.reads
    );
    // A drive that is ready at once costs no window at all.
    let mut ready = FakeAta::new(|_| 0x58);
    check!(ata::wait_for_data_on(&mut ready), "a ready drive failed");
    check!(ready.paces == 0, "{} paces on a ready drive", ready.paces);
    Ok(())
}

/// Stress: thousands of probes against random status streams all end within
/// the bound and never report a drive for a stream of `0xFF`.
pub fn ata_probe_soak() -> Result<(), String> {
    let mut seed = 0xA5A5_5A5A_DEAD_BEEFu64;
    for round in 0..3000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let first = (seed & 0xFF) as u8;
        let rest = ((seed >> 8) & 0xFF) as u8;
        // Short streams only: a status that never settles would take the
        // full bound, which `ata_waits_are_bounded` already proves.
        let settle = (seed >> 16) % 8;
        let mut fake = FakeAta::new(move |read| match read {
            1 => first,
            n if u64::from(n) < 2 + settle => rest | 0x80,
            _ => 0x58,
        });
        let found = ata::identify_on(&mut fake);
        check!(
            fake.reads <= 3 + 2 * ata::POLL_LIMIT,
            "round {round}: {} reads",
            fake.reads
        );
        if ata::status_means_absent(first) {
            check!(
                found.is_none() && fake.reads == 1,
                "round {round}: absent first status"
            );
        }
    }
    Ok(())
}

/// A fake i8042: status bytes come from a closure; replies queue up.
struct FakeI8042<F: FnMut(&[u8]) -> u8> {
    status: F,
    replies: Vec<u8>,
    commands: Vec<u8>,
    reads: u32,
}

impl<F: FnMut(&[u8]) -> u8> Controller for FakeI8042<F> {
    fn status(&mut self) -> u8 {
        self.reads += 1;
        (self.status)(&self.replies)
    }
    fn read_data(&mut self) -> u8 {
        if self.replies.is_empty() {
            0xFF
        } else {
            self.replies.remove(0)
        }
    }
    fn command(&mut self, command: u8) {
        self.commands.push(command);
        match command {
            0x20 => self.replies.push(0x47),
            0xA9 => self.replies.push(0x00),
            _ => {}
        }
    }
}

fn fake_i8042<F: FnMut(&[u8]) -> u8>(status: F) -> FakeI8042<F> {
    FakeI8042 {
        status,
        replies: Vec::new(),
        commands: Vec::new(),
        reads: 0,
    }
}

/// No controller (`0xFF`) is absent after one read; a working controller is
/// present with its aux port; one that never answers is absent within the
/// bound; one with stale bytes is drained first.
pub fn i8042_probe_cases() -> Result<(), String> {
    let mut none = fake_i8042(|_| 0xFF);
    check!(
        matches!(i8042::probe_on(&mut none), Probe::Absent(_)),
        "0xFF present"
    );
    check!(
        none.reads == 1 && none.commands.is_empty(),
        "0xFF: {} reads",
        none.reads
    );

    let ready = |replies: &[u8]| if replies.is_empty() { 0x1C } else { 0x1D };
    let mut real = fake_i8042(ready);
    check!(
        i8042::probe_on(&mut real) == Probe::Present { aux: true },
        "a working controller was not found"
    );
    check!(
        real.commands == [0x20, 0xA9],
        "commands {:x?}",
        real.commands
    );

    let mut stale = fake_i8042(ready);
    stale.replies.extend_from_slice(&[0xFA, 0xAA, 0x1C]);
    check!(
        matches!(i8042::probe_on(&mut stale), Probe::Present { .. }),
        "stale bytes"
    );

    // Input buffer full forever (a wedged controller): absent, bounded.
    let mut wedged = fake_i8042(|_| 0x1E);
    check!(
        matches!(i8042::probe_on(&mut wedged), Probe::Absent(_)),
        "wedged present"
    );
    check!(
        wedged.reads <= 4 + i8042::PROBE_POLLS,
        "wedged took {}",
        wedged.reads
    );

    // Never replies to read-config: absent, bounded.
    let mut silent = fake_i8042(|_| 0x1C);
    let probe = i8042::probe_on(&mut SilentReplies(&mut silent));
    check!(
        matches!(probe, Probe::Absent(_)),
        "a mute controller is present: {probe:?}"
    );
    check!(
        silent.reads <= 4 + i8042::PROBE_POLLS,
        "mute took {}",
        silent.reads
    );
    // The live probe under QEMU found the emulated controller.
    check!(i8042::present(), "QEMU's i8042 was not found at boot");
    Ok(())
}

/// Wraps a fake so its commands get no reply (a controller that ignores us).
struct SilentReplies<'a, F: FnMut(&[u8]) -> u8>(&'a mut FakeI8042<F>);

impl<F: FnMut(&[u8]) -> u8> Controller for SilentReplies<'_, F> {
    fn status(&mut self) -> u8 {
        self.0.status()
    }
    fn read_data(&mut self) -> u8 {
        self.0.read_data()
    }
    fn command(&mut self, command: u8) {
        self.0.commands.push(command);
    }
}

/// A fake UART: `answers` decides whether the scratch register keeps writes.
struct FakeUart {
    line_status: u8,
    scratch: u8,
    keeps: bool,
}

impl Uart for FakeUart {
    fn read(&mut self, register: u16) -> u8 {
        match register {
            5 => self.line_status,
            7 if self.keeps => self.scratch,
            _ => 0xFF,
        }
    }
    fn write(&mut self, register: u16, value: u8) {
        if register == 7 {
            self.scratch = value;
        }
    }
}

/// COM1: a missing port (all `0xFF`) and a port whose scratch register does
/// not hold are absent; a 16550 is present and gets its scratch value back.
pub fn com1_probe_cases() -> Result<(), String> {
    let mut missing = FakeUart {
        line_status: 0xFF,
        scratch: 0,
        keeps: false,
    };
    check!(
        !serial::probe_on(&mut missing),
        "a floating COM1 is present"
    );
    let mut deaf = FakeUart {
        line_status: 0x60,
        scratch: 0,
        keeps: false,
    };
    check!(
        !serial::probe_on(&mut deaf),
        "a scratch-less port is present"
    );
    let mut uart = FakeUart {
        line_status: 0x60,
        scratch: 0x42,
        keeps: true,
    };
    check!(serial::probe_on(&mut uart), "a 16550 is absent");
    check!(
        uart.scratch == 0x42,
        "scratch not restored: {:#x}",
        uart.scratch
    );
    // QEMU's COM1 (the harness reads this very line from it).
    check!(serial::present(), "QEMU's COM1 was not found at boot");
    Ok(())
}

/// The transmit ring (P5): wraps and drains for room without losing order,
/// honours a drain budget; and a program-sized mirror of several ring's
/// worth, interleaved with kernel lines, reaches the port (the harness reads
/// the lines after this one, so a lost or reordered byte breaks the run).
pub fn com1_ring_cases() -> Result<(), String> {
    serial::ring_selftest().map_err(String::from)?;
    let mut text = Vec::new();
    for line in 0..300 {
        text.extend_from_slice(format!("SERIAL:mirror:{line:04}:{}\n", "x".repeat(40)).as_bytes());
        if text.len() > 8192 {
            serial::mirror(&text);
            text.clear();
            serial_println!("TEST:fw_com1_ring_cases:INFO:kernel line between mirrors");
        }
    }
    serial::mirror(&text);
    serial::flush();
    Ok(())
}

/// `syslog(2)` (Linux 103, behind BusyBox `dmesg`): the size query, a read
/// that returns the newest ring bytes ending with the line just logged, the
/// refused and invalid actions, and a soak of interleaved logging and reads.
pub fn syslog_reads_the_ring() -> Result<(), String> {
    const SYSLOG: u64 = 103;
    const READ_ALL: u64 = 3;
    const SIZE_BUFFER: u64 = 10;
    let call = |action: u64, buf: u64, len: u64| {
        crate::process::linux::dispatch_for_test(SYSLOG, action, buf, len) as i64
    };
    check!(
        call(SIZE_BUFFER, 0, 0) == crate::klog::CAPACITY as i64,
        "size query"
    );
    check!(call(READ_ALL, 0, 16) == -22, "a null buffer was accepted");
    check!(call(5, 0, 0) == -1, "clearing the ring was not refused");
    check!(call(77, 0, 0) == -22, "an unknown action was accepted");
    let mut buf = vec![0u8; 4096];
    for round in 0..200u32 {
        let marker = format!("syslog soak marker {round}\n");
        crate::serial_print!("{marker}");
        let len = 64 + (round as usize * 37) % 4000;
        let got = call(READ_ALL, buf.as_mut_ptr() as u64, len as u64);
        check!(got == len as i64, "round {round}: read {got} of {len}");
        check!(
            buf[..len].ends_with(marker.as_bytes()),
            "round {round}: the read does not end with the newest line"
        );
    }
    Ok(())
}
