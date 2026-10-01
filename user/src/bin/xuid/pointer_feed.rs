//! The pointer from `inputd` (`docs/usb-hid-plan.md`, phase P2).
//!
//! Once attached, `xuid` stops reading pointer records from the kernel's
//! display stream and takes the one cursor `inputd` keeps for every pointing
//! device. Each `PointerEvent` (absolute position, held-button mask, wheel)
//! becomes the compositor's existing internal events, so hit-testing, drags,
//! focus-on-click and the frozen `display.v1` client events are unchanged:
//! a move when the position changed, then the wheel, then one press or
//! release per button whose bit flipped, in the order `PointerEvent` promises.

use alloc::vec::Vec;

use user::messenger::input::PointerState;

use super::compositor::Compositor;
use super::protocol::{Event, EventKind};

/// Display button ids `xuid` forwards: 1 left, 2 right, 3 middle (the ids
/// `display.v1` documents; bit `n` of the mask is button `n + 1`).
const BUTTONS: u32 = 3;

/// The events that take the compositor from `(pointer, held)` to `state`.
pub(super) fn translate(
    pointer: (i32, i32),
    held: u32,
    state: &PointerState,
    out: &mut Vec<Event>,
) {
    if (state.x, state.y) != pointer {
        out.push(event(EventKind::PointerMove, state.x, state.y));
    }
    if state.wheel != 0 {
        out.push(event(EventKind::PointerWheel, state.wheel, 0));
    }
    for button in 1..=BUTTONS {
        let bit = 1 << (button - 1);
        if (held ^ state.buttons) & bit != 0 {
            let kind = if state.buttons & bit != 0 {
                EventKind::PointerDown
            } else {
                EventKind::PointerUp
            };
            out.push(event(kind, button as i32, 0));
        }
    }
}

fn event(kind: EventKind, a: i32, b: i32) -> Event {
    Event {
        kind,
        a: i64::from(a),
        b: i64::from(b),
    }
}

/// The forwarded-button bits of a mask.
pub(super) fn forwarded(buttons: u32) -> u32 {
    buttons & ((1 << BUTTONS) - 1)
}

impl Compositor {
    /// Apply one `PointerEvent` (or the `GetPointer` seed) from `inputd`.
    pub(super) fn apply_pointer(&mut self, state: &PointerState) {
        let mut events = Vec::new();
        translate(self.pointer, self.input.buttons, state, &mut events);
        self.input.buttons = forwarded(state.buttons);
        for event in events {
            self.handle_event(event);
        }
    }

    /// `inputd` stopped owning the pointer: release what it held, so nothing
    /// stays pressed while the kernel stream takes over again.
    pub(super) fn release_pointer(&mut self) {
        let released = PointerState {
            x: self.pointer.0,
            y: self.pointer.1,
            ..PointerState::default()
        };
        self.apply_pointer(&released);
    }
}

/// One self-test case: start pointer, held mask, input, expected `(kind, a)`.
type Case = ((i32, i32), u32, PointerState, &'static [(EventKind, i64)]);

/// Boot self-test of [`translate`]: `XUID:POINTER:PASS`, or the first wrong case.
pub(super) fn selftest_pointer_feed() -> &'static str {
    use EventKind::{PointerDown, PointerMove, PointerUp, PointerWheel};
    let state = |x, y, buttons, wheel| PointerState {
        x,
        y,
        buttons,
        wheel,
        wheel_h: 0,
    };
    let cases: [Case; 5] = [
        // Nothing changed: nothing to do.
        ((5, 5), 0, state(5, 5, 0, 0), &[]),
        // Move, wheel, then the press, in that order.
        (
            (0, 0),
            0,
            state(3, 4, 1, 2),
            &[(PointerMove, 3), (PointerWheel, 2), (PointerDown, 1)],
        ),
        // Swap left for right: release left, press right.
        (
            (3, 4),
            1,
            state(3, 4, 2, 0),
            &[(PointerUp, 1), (PointerDown, 2)],
        ),
        // Back and forward stay with `inputd`: not a display.v1 button.
        ((3, 4), 0, state(3, 4, 0x18, 0), &[]),
        // Middle, with a downward wheel notch before it.
        (
            (3, 4),
            0,
            state(3, 4, 4, -1),
            &[(PointerWheel, -1), (PointerDown, 3)],
        ),
    ];
    for (pointer, held, input, want) in cases {
        let mut out = Vec::new();
        translate(pointer, held, &input, &mut out);
        let got: Vec<(EventKind, i64)> = out.iter().map(|e| (e.kind, e.a)).collect();
        if got != want {
            return "XUID:POINTER:FAIL translate\n";
        }
    }
    "XUID:POINTER:PASS\n"
}
