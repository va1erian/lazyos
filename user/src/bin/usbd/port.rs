//! Root hub ports (xHCI 4.3.1, 4.19; USB 2.0 7.1.7.3 and 7.1.7.5; USB 3.2
//! 7.5): power, the connect debounce, reset and recovery on USB 2 ports,
//! link training and warm reset on USB 3 ports.
//!
//! Timings are the specification's minimums, not QEMU's tolerance: a real
//! keyboard is not ready for its first request until the reset recovery
//! time has passed. The same constants serve ports of external hubs.

use xhci::regs::{portsc, Speed};

use super::hc::{nap, sleep_ms, Hc};
use user::sys;

/// TATTDB: a connect must stay stable this long before the port is reset.
pub(super) const DEBOUNCE_MS: u64 = 100;
/// TRSTRCY: after a reset the device gets this long before any request.
pub(super) const RESET_RECOVERY_MS: u64 = 10;
/// Port power to power good on a root port (xHCI reports 20 ms).
const POWER_ON_MS: u64 = 20;
/// How long a reset may take before the port is given up (the controller
/// drives a USB 2 root port reset for 50 ms itself; a warm reset is longer).
const RESET_TICKS: u64 = 50;
/// How long a USB 3 link may train after a connect (Polling lasts up to
/// 360 ms, then the device may still need its own recovery).
const TRAINING_TICKS: u64 = 100;

/// Turn on every root port that has a power switch and is off. Returns
/// whether any was switched on (after waiting for power to be good).
pub(super) fn power_on(hc: &mut Hc) -> bool {
    if !hc.info.port_power {
        return false;
    }
    let mut switched = false;
    for port in 1..=hc.info.ports {
        let status = hc.portsc(port);
        if status & portsc::PP == 0 {
            hc.set_portsc(port, portsc::set(status, portsc::PP));
            switched = true;
        }
    }
    if switched {
        sleep_ms(POWER_ON_MS);
    }
    switched
}

/// Whether root `port` is a USB 3 port: the Supported Protocol table when
/// the controller has one, else the speed it reports.
pub(super) fn is_usb3(hc: &Hc, port: u8) -> bool {
    match hc.ports.major(port) {
        Some(major) => major == 3,
        None => matches!(
            Speed::of_port(hc.portsc(port)),
            Some(Speed::Super | Speed::SuperPlus)
        ),
    }
}

/// Bring a connected root port to Enabled and return the speed its device
/// runs at; `None` when nothing usable is there. USB 2 ports are debounced,
/// reset and given their recovery time; USB 3 ports train on their own and
/// get a warm reset only when the link is stuck.
pub(super) fn enable(hc: &mut Hc, port: u8) -> Option<Speed> {
    if hc.portsc(port) & portsc::CCS == 0 {
        return None;
    }
    let enabled = if is_usb3(hc, port) {
        enable_usb3(hc, port)
    } else {
        enable_usb2(hc, port)
    };
    let status = hc.portsc(port);
    hc.set_portsc(port, portsc::ack_changes(status));
    if !enabled || status & (portsc::CCS | portsc::PED) != portsc::CCS | portsc::PED {
        return None;
    }
    // The speed is only final once the port is enabled.
    let psiv = ((status & portsc::SPEED_MASK) >> portsc::SPEED_SHIFT) as u8;
    hc.ports.speed(port, psiv)
}

fn enable_usb2(hc: &mut Hc, port: u8) -> bool {
    sleep_ms(DEBOUNCE_MS);
    let status = hc.portsc(port);
    if status & portsc::CCS == 0 {
        return false;
    }
    hc.set_portsc(port, portsc::set(status, portsc::PR));
    let done = |s: u32| s & portsc::PRC != 0 && s & portsc::PR == 0;
    if !wait_port(hc, port, RESET_TICKS, true, done) {
        return false;
    }
    sleep_ms(RESET_RECOVERY_MS);
    true
}

fn enable_usb3(hc: &mut Hc, port: u8) -> bool {
    let trained = wait_port(hc, port, TRAINING_TICKS, true, |s| {
        s & portsc::PED != 0
            || matches!(
                portsc::link_state(s),
                portsc::link::INACTIVE | portsc::link::COMPLIANCE
            )
    });
    let status = hc.portsc(port);
    if trained && status & portsc::PED != 0 {
        return true;
    }
    // Stuck in training, SS.Inactive or Compliance Mode: only a warm reset
    // brings the link back (xHCI 4.19.5.1).
    hc.set_portsc(port, portsc::set(status, portsc::WPR));
    // The link drops through Rx.Detect during a warm reset, so a passing
    // disconnect does not end the wait.
    let reset = wait_port(hc, port, RESET_TICKS, false, |s| {
        s & (portsc::WRC | portsc::PRC) != 0 && s & portsc::PR == 0
    });
    reset && wait_port(hc, port, RESET_TICKS, false, |s| s & portsc::PED != 0)
}

/// Poll `PORTSC` of `port` up to `ticks` until `done`; `false` on timeout or,
/// with `connected`, when the device went away meanwhile.
fn wait_port(hc: &Hc, port: u8, ticks: u64, connected: bool, done: impl Fn(u32) -> bool) -> bool {
    let deadline = sys::clock() + ticks;
    loop {
        let status = hc.portsc(port);
        if done(status) {
            return true;
        }
        if (connected && status & portsc::CCS == 0) || sys::clock() > deadline {
            return false;
        }
        nap();
    }
}
