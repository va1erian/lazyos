//! i8042 (PS/2 controller) presence probe (H1 of `docs/real-pc-boot-plan.md`).
//!
//! Desktop boards since about 2015 often have no PS/2 controller at all; its
//! ports then float and read `0xFF`, and the mouse setup's waits would each
//! run to their bound for nothing. So before anything talks to the
//! controller, [`init`] asks it two cheap questions with short, bounded
//! waits: can its command byte be read back, and does the auxiliary port
//! pass its self-test. The answer gates `mouse::init` and the IRQ1/IRQ12
//! lines, and is logged as `HW:I8042:PRESENT` or `HW:I8042:ABSENT`. A machine
//! whose firmware emulates PS/2 for a USB keyboard (SMM legacy support)
//! answers like a real controller, which is what it behaves as.
//!
//! The probe never resets the controller (`0xAA` can clear the configuration
//! the firmware set up) and never touches the keyboard device itself.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::io::{inb, outb};

const DATA: u16 = 0x60;
const STATUS_COMMAND: u16 = 0x64;
/// Status: the output buffer holds a byte for the host.
const OUTPUT_FULL: u8 = 0x01;
/// Status: the controller has not consumed the last byte written to it.
const INPUT_FULL: u8 = 0x02;
/// What an undecoded port reads.
const FLOATING: u8 = 0xFF;
/// Controller commands.
const READ_CONFIG: u8 = 0x20;
const TEST_AUX: u8 = 0xA9;
/// Status reads a probe step may take: a real controller answers in well
/// under a millisecond; at ~1us per port read this is ~20ms per step.
pub const PROBE_POLLS: u32 = 20_000;
/// Stale bytes drained before probing (the controller buffers one or two).
const FLUSH_LIMIT: u32 = 32;

/// The controller's two ports, so the probe can run against fakes.
pub trait Controller {
    fn status(&mut self) -> u8;
    fn read_data(&mut self) -> u8;
    fn command(&mut self, command: u8);
}

/// The real i8042 at 0x60/0x64.
pub struct Ports;

impl Controller for Ports {
    fn status(&mut self) -> u8 {
        // SAFETY: reading the i8042 status register has no side effect.
        unsafe { inb(STATUS_COMMAND) }
    }

    fn read_data(&mut self) -> u8 {
        // SAFETY: reading the data port consumes one buffered byte, which is
        // what draining and reading a reply mean.
        unsafe { inb(DATA) }
    }

    fn command(&mut self, command: u8) {
        // SAFETY: the probe only sends read-config and the aux self-test,
        // neither of which changes the controller's configuration.
        unsafe { outb(STATUS_COMMAND, command) };
    }
}

/// What the probe found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Probe {
    /// No controller; the reason names the step that failed.
    Absent(&'static str),
    /// A controller; `aux` is whether its mouse port passed its self-test.
    Present { aux: bool },
}

fn wait_input_clear(controller: &mut impl Controller) -> bool {
    (0..PROBE_POLLS).any(|_| controller.status() & INPUT_FULL == 0)
}

fn wait_output_full(controller: &mut impl Controller) -> bool {
    (0..PROBE_POLLS).any(|_| controller.status() & OUTPUT_FULL != 0)
}

/// Send `command` and return the controller's one-byte reply, if it gives one.
fn ask(controller: &mut impl Controller, command: u8) -> Option<u8> {
    if !wait_input_clear(controller) {
        return None;
    }
    controller.command(command);
    wait_output_full(controller).then(|| controller.read_data())
}

/// Probe for a controller. Every loop is bounded, so the worst case (a port
/// that answers "busy" forever) costs a few [`PROBE_POLLS`] reads.
pub fn probe_on(controller: &mut impl Controller) -> Probe {
    if controller.status() == FLOATING {
        return Probe::Absent("status reads 0xff");
    }
    for _ in 0..FLUSH_LIMIT {
        if controller.status() & OUTPUT_FULL == 0 {
            break;
        }
        controller.read_data();
    }
    if controller.status() & OUTPUT_FULL != 0 {
        return Probe::Absent("output buffer never drains");
    }
    if ask(controller, READ_CONFIG).is_none() {
        return Probe::Absent("no reply to read-config");
    }
    Probe::Present {
        aux: ask(controller, TEST_AUX) == Some(0x00),
    }
}

static PRESENT: AtomicBool = AtomicBool::new(false);

/// Whether the boot probe found a controller.
#[allow(dead_code)] // Read by the kernel suite; `usbd` policy is the next reader.
pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

/// Probe the controller, set up the PS/2 mouse when there is one, and mask
/// the PS/2 interrupt lines that have nothing behind them.
pub fn init() {
    let probe = probe_on(&mut Ports);
    PRESENT.store(matches!(probe, Probe::Present { .. }), Ordering::Relaxed);
    match probe {
        Probe::Present { aux } => {
            serial_println!("HW:I8042:PRESENT aux={}", if aux { "yes" } else { "no" });
            if aux {
                super::mouse::init();
            } else {
                crate::arch::pic::set_masked(12, true);
            }
        }
        Probe::Absent(reason) => {
            serial_println!("HW:I8042:ABSENT ({reason})");
            crate::arch::pic::set_masked(1, true);
            crate::arch::pic::set_masked(12, true);
        }
    }
}
