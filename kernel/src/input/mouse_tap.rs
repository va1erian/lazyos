//! The PS/2 mouse driver's tap onto the raw event bus.
//!
//! [`MouseTap`] turns each decoded packet into device-independent pointer
//! records (`bus::pointer`): one `REL_MOTION` when the mouse moved, one
//! vertical `SCROLL` when the wheel turned, and a `BUTTON` edge per button
//! that changed, in that order, so a press lands at the packet's position and
//! after its wheel, as `PointerEvent` promises.
//! It carries no policy: no cursor, no clamping, no acceleration (that is
//! `inputd`, `docs/usb-hid-plan.md` decision 1). It runs beside the legacy
//! display-grant stream in `mouse.rs` until that path is retired.

use spin::Mutex;

use super::bus::{self, device, kind, pointer, value};
use super::mouse::Decoded;

/// The tap the IRQ12 handler feeds.
pub static TAP: Mutex<MouseTap> = Mutex::new(MouseTap::new());

/// The buttons a PS/2 packet reports, with their HID usages.
const BUTTONS: [u16; 3] = [
    pointer::button::LEFT,
    pointer::button::RIGHT,
    pointer::button::MIDDLE,
];

pub struct MouseTap {
    /// Bit `n` is set while `BUTTONS[n]` is held.
    held: u8,
}

impl MouseTap {
    pub const fn new() -> Self {
        MouseTap { held: 0 }
    }

    /// Forget held buttons (test hook).
    #[cfg(lazyos_tests)]
    pub fn reset(&mut self) {
        self.held = 0;
    }

    /// Publish the records one decoded packet means.
    pub fn feed(&mut self, packet: &Decoded) {
        // PS/2 motion is 9-bit, so it always fits an `i16`; Y is positive
        // upwards on the wire and downwards on the bus.
        let (dx, dy) = (packet.dx as i16, (-packet.dy) as i16);
        if dx != 0 || dy != 0 {
            emit(kind::REL_MOTION, 0, pointer::pack_rel(dx, dy));
        }
        if packet.wheel != 0 {
            emit(kind::SCROLL, pointer::VERTICAL, packet.wheel);
        }
        let now =
            u8::from(packet.left) | u8::from(packet.right) << 1 | u8::from(packet.middle) << 2;
        for (bit, usage) in BUTTONS.iter().enumerate() {
            let mask = 1 << bit;
            if (self.held ^ now) & mask != 0 {
                let state = if now & mask != 0 {
                    value::PRESS
                } else {
                    value::RELEASE
                };
                emit(kind::BUTTON, *usage, state);
            }
        }
        self.held = now;
    }
}

fn emit(kind: u8, code: u16, value: i32) {
    bus::publish(device::PS2_MOUSE, kind, code, value);
}
