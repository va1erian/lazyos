//! The i8042 intake (`input::ps2`): bytes are collected in controller order
//! while interrupts are off, retained up to `FIFO_CAP` with nothing decoding
//! them, refused and counted past that, and a loss releases every held key.
//!
//! The controller tests feed real bytes through the i8042's own "write output
//! buffer" commands (0xD2 keyboard, 0xD3 auxiliary), so the status-and-data
//! path is the hardware one, not a stub.

use super::*;
use crate::arch::io::{inb, outb};
use crate::input::ps2::{self, Byte, Fifo, Port, FIFO_CAP};
use x86_64::instructions::interrupts::without_interrupts;

/// i8042 command: put the next data byte in the output buffer as keyboard
/// (0xD2) or auxiliary-device (0xD3) data.
const WRITE_KBD_OBUF: u8 = 0xD2;
const WRITE_AUX_OBUF: u8 = 0xD3;

/// Wait until the controller accepts a command/data byte.
fn wait_input_empty() -> Result<(), String> {
    for _ in 0..1_000_000 {
        // SAFETY: reading the i8042 status register has no side effect.
        if unsafe { inb(0x64) } & 0x02 == 0 {
            return Ok(());
        }
    }
    Err("i8042 input buffer never drained".into())
}

/// Make the controller present `byte` as if `port`'s device had sent it.
/// Interrupts must be off, or IRQ1/IRQ12 would consume it first.
fn inject(port: Port, byte: u8) -> Result<(), String> {
    let command = match port {
        Port::Keyboard => WRITE_KBD_OBUF,
        Port::Mouse => WRITE_AUX_OBUF,
    };
    wait_input_empty()?;
    // SAFETY: 0xD2/0xD3 followed by one data byte is the documented i8042
    // sequence; it only fills the controller's output buffer.
    unsafe { outb(0x64, command) };
    wait_input_empty()?;
    // SAFETY: the data byte the command above asked for.
    unsafe { outb(0x60, byte) };
    Ok(())
}

/// Pop everything the FIFO holds.
fn take_fifo() -> Vec<Byte> {
    ps2::with_fifo(|fifo| core::iter::from_fn(|| fifo.pop()).collect())
}

fn key(value: u8) -> Byte {
    Byte {
        port: Port::Keyboard,
        value,
    }
}

/// Order, exact capacity, the refused byte is the newest, and the count.
pub fn fifo_order_and_capacity() -> Result<(), String> {
    let mut fifo = alloc::boxed::Box::new(Fifo::new());
    for index in 0..FIFO_CAP {
        check!(
            fifo.push(key(index as u8)),
            "byte {index} refused below cap"
        );
    }
    check!(fifo.len() == FIFO_CAP, "len {} after filling", fifo.len());
    check!(!fifo.push(key(0xEE)), "a full FIFO accepted a byte");
    check!(!fifo.push(key(0xEF)), "a full FIFO accepted a byte");
    check!(fifo.take_dropped() == 2, "two refusals not counted");
    check!(fifo.take_dropped() == 0, "the drop count did not reset");
    for index in 0..FIFO_CAP {
        let got = fifo.pop();
        check!(
            got == Some(key(index as u8)),
            "pop {index} gave {got:?}: order or the refused byte leaked in"
        );
    }
    check!(fifo.pop().is_none(), "FIFO not empty after draining");
    check!(fifo.peak() == FIFO_CAP, "peak {}", fifo.peak());
    // Wrap-around keeps order.
    for round in 0..3 * FIFO_CAP {
        check!(fifo.push(key(round as u8)), "push {round}");
        check!(fifo.pop() == Some(key(round as u8)), "pop {round}");
    }
    Ok(())
}

/// Bytes the controller holds are collected in order with their port, while
/// interrupts stay off, and nothing is decoded until dispatch.
pub fn controller_bytes_collected_in_order() -> Result<(), String> {
    fresh();
    ps2::reset();
    let sent = [
        (Port::Keyboard, 0x1E),
        (Port::Mouse, 0x08),
        (Port::Keyboard, 0x9E),
        (Port::Mouse, 0x01),
        (Port::Mouse, 0x02),
        (Port::Keyboard, 0xE0),
        (Port::Keyboard, 0x48),
    ];
    let collected = without_interrupts(|| -> Result<Vec<Byte>, String> {
        for (port, value) in sent {
            inject(port, value)?;
            ps2::service();
        }
        Ok(take_fifo())
    })?;
    let want: Vec<Byte> = sent
        .iter()
        .map(|&(port, value)| Byte { port, value })
        .collect();
    check!(collected == want, "collected {collected:?}, want {want:?}");
    ps2::reset();
    fresh();
    Ok(())
}

/// A held key whose release was lost is released by the loss itself: the
/// FIFO overflows with the key's typematic make codes, and the consumer sees
/// exactly one press and one release, plus the counted drop.
pub fn overflow_is_counted_and_releases_held_keys() -> Result<(), String> {
    fresh();
    ps2::reset();
    let owner = scratch()?;
    bus::open(owner).map_err(|e| format!("{e:?}"))?;
    let before = ps2::dropped_total();
    const EXTRA: usize = 37;
    without_interrupts(|| {
        ps2::with_fifo(|fifo| {
            // 'A' pressed, then held: the hardware repeats its make code.
            for _ in 0..FIFO_CAP + EXTRA {
                fifo.push(key(0x1E));
            }
        });
        ps2::mark_pending();
        ps2::dispatch();
    });
    let lost = ps2::dropped_total() - before;
    check!(
        lost == EXTRA as u64,
        "{lost} bytes counted lost, want {EXTRA}"
    );
    let records = drain_all(owner, 64)?;
    let keys: Vec<(u16, i32)> = records.iter().map(|r| (r.code, r.value)).collect();
    check!(
        keys == [(0x04, bus::value::PRESS), (0x04, bus::value::RELEASE)],
        "consumer saw {keys:?}"
    );
    bus::close(bus::consumer_of(owner).ok_or("no consumer")?, owner)
        .map_err(|e| format!("{e:?}"))?;
    ps2::reset();
    fresh();
    Ok(())
}

/// A small deterministic generator, so a failure reproduces.
struct Rng(u64);

impl Rng {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }
}

/// Letter keys: (set-1 make code, HID usage).
const LETTERS: [(u8, u16); 8] = [
    (0x1E, 0x04),
    (0x1F, 0x16),
    (0x20, 0x07),
    (0x21, 0x09),
    (0x24, 0x0D),
    (0x25, 0x0E),
    (0x26, 0x0F),
    (0x10, 0x14),
];

/// Soak: thousands of keystrokes through the real controller while
/// interrupts stay off for long stretches (a syscall writing a big file), the
/// intake servicing between bytes and the decoder held back for up to four
/// rounds (a descheduled consumer). Every press and release arrives, in
/// order, with no loss anywhere.
pub fn soak_interrupts_off_bursts_lose_nothing() -> Result<(), String> {
    const ROUNDS: usize = 400;
    fresh();
    ps2::reset();
    let owner = scratch()?;
    bus::open(owner).map_err(|e| format!("{e:?}"))?;
    let lost_before = ps2::dropped_total();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut want: Vec<(u16, i32)> = Vec::new();
    let mut got: Vec<(u16, i32)> = Vec::new();
    let mut held_back = 0;
    for round in 0..ROUNDS {
        // Up to 30 keystrokes (60 bytes) per round; at most four rounds
        // undecoded is 240 bus events, inside the consumer ring.
        let strokes = 1 + rng.below(30) as usize;
        without_interrupts(|| -> Result<(), String> {
            for _ in 0..strokes {
                let (make, usage) = LETTERS[rng.below(LETTERS.len() as u64) as usize];
                for (byte, value) in [
                    (make, bus::value::PRESS),
                    (make | 0x80, bus::value::RELEASE),
                ] {
                    inject(Port::Keyboard, byte)?;
                    ps2::service();
                    want.push((usage, value));
                }
            }
            Ok(())
        })?;
        if held_back < 3 && rng.below(2) == 0 {
            held_back += 1;
            continue;
        }
        held_back = 0;
        // Decode in interrupt-like context (interrupts off), as IRQ1 does.
        without_interrupts(ps2::dispatch);
        let records = drain_all(owner, 64)?;
        check!(
            records.iter().all(|r| r.kind == kind::KEY),
            "round {round}: a non-key record (loss marker?) arrived"
        );
        got.extend(records.iter().map(|r| (r.code, r.value)));
    }
    without_interrupts(ps2::dispatch);
    got.extend(drain_all(owner, 64)?.iter().map(|r| (r.code, r.value)));
    check!(
        ps2::dropped_total() == lost_before,
        "the intake dropped {} bytes",
        ps2::dropped_total() - lost_before
    );
    check!(
        got.len() == want.len(),
        "{} key edges arrived of {}",
        got.len(),
        want.len()
    );
    if let Some(index) = got.iter().zip(&want).position(|(a, b)| a != b) {
        return Err(format!(
            "edge {index}: got {:?}, want {:?}",
            got[index], want[index]
        ));
    }
    bus::close(bus::consumer_of(owner).ok_or("no consumer")?, owner)
        .map_err(|e| format!("{e:?}"))?;
    ps2::reset();
    fresh();
    Ok(())
}
