//! The pointer from `inputd` (`docs/usb-hid-plan.md`, phase P2).
//!
//! Once attached, `xuid` stops reading pointer records from the kernel's
//! display stream and takes the one cursor `inputd` keeps for every pointing
//! device. Each `PointerEvent` (absolute position, held-button mask, wheel)
//! becomes the compositor's existing internal events, so hit-testing, drags,
//! focus-on-click and the frozen `display.v1` client events are unchanged:
//! a move when the position changed, then the wheel, then one press or
//! release per button whose bit flipped, in the order `PointerEvent` promises.

use user::messenger::input::PointerState;

use super::compositor::Compositor;
use super::protocol::{Event, EventKind};

/// Display button ids `xuid` forwards: 1 left, 2 right, 3 middle (the ids
/// `display.v1` documents; bit `n` of the mask is button `n + 1`).
const BUTTONS: u32 = 3;
/// The most events one `PointerEvent` becomes: a move, the wheel and one
/// edge per forwarded button.
pub(super) const MAX_EVENTS: usize = 2 + BUTTONS as usize;

/// The events one `PointerEvent` became, in order, and how many. A fixed
/// array: pointer events arrive at input rates and the user bump allocator
/// never reclaims.
pub(super) type Translated = ([Event; MAX_EVENTS], usize);

/// The events that take the compositor from `(pointer, held)` to `state`.
pub(super) fn translate(pointer: (i32, i32), held: u32, state: &PointerState) -> Translated {
    let mut out = ([event(EventKind::PointerMove, 0, 0); MAX_EVENTS], 0);
    let mut push = |event: Event| {
        out.0[out.1] = event;
        out.1 += 1;
    };
    if (state.x, state.y) != pointer {
        push(event(EventKind::PointerMove, state.x, state.y));
    }
    if state.wheel != 0 {
        push(event(EventKind::PointerWheel, state.wheel, 0));
    }
    for button in 1..=BUTTONS {
        let bit = 1 << (button - 1);
        if (held ^ state.buttons) & bit != 0 {
            let kind = if state.buttons & bit != 0 {
                EventKind::PointerDown
            } else {
                EventKind::PointerUp
            };
            push(event(kind, button as i32, 0));
        }
    }
    out
}

fn event(kind: EventKind, a: i32, b: i32) -> Event {
    Event {
        kind,
        a: i64::from(a),
        b: i64::from(b),
    }
}

/// Whether `newer` makes `older` redundant: `older` only moved the pointer
/// (no wheel) and `newer` holds the same buttons, so applying `newer` alone
/// lands in the same state with the same edges (docs/performance-plan.md
/// P3.4).
pub(super) fn supersedes(older: &PointerState, newer: &PointerState) -> bool {
    older.wheel == 0 && older.wheel_h == 0 && older.buttons == newer.buttons
}

/// The forwarded-button bits of a mask.
pub(super) fn forwarded(buttons: u32) -> u32 {
    buttons & ((1 << BUTTONS) - 1)
}

impl Compositor {
    /// The events a `PointerEvent` from `inputd` means, relative to the
    /// newest pointer state seen (held input included), recording its
    /// buttons as the new held mask.
    pub(super) fn translate_pointer(&mut self, state: &PointerState) -> Translated {
        let from = self.held.pointer(self.pointer);
        let events = translate(from, self.input.buttons, state);
        self.input.buttons = forwarded(state.buttons);
        events
    }

    /// Apply one `PointerEvent` (or the `GetPointer` seed) from `inputd`,
    /// behind any input an animation held.
    pub(super) fn apply_pointer(&mut self, state: &PointerState) {
        let (events, count) = self.translate_pointer(state);
        for event in &events[..count] {
            self.dispatch(*event);
        }
    }

    /// `inputd` stopped owning the pointer: release what it held, so nothing
    /// stays pressed while the kernel stream takes over again.
    pub(super) fn release_pointer(&mut self) {
        let (x, y) = self.held.pointer(self.pointer);
        self.apply_pointer(&PointerState {
            x,
            y,
            ..PointerState::default()
        });
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
    let cases: [Case; 6] = [
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
        // The fullest case fits the fixed buffer.
        (
            (0, 0),
            0,
            state(1, 1, 7, 1),
            &[
                (PointerMove, 1),
                (PointerWheel, 1),
                (PointerDown, 1),
                (PointerDown, 2),
                (PointerDown, 3),
            ],
        ),
    ];
    for (pointer, held, input, want) in cases {
        let (out, count) = translate(pointer, held, &input);
        let got = &out[..count];
        if got.len() != want.len() || got.iter().zip(want).any(|(e, w)| (e.kind, e.a) != *w) {
            return "XUID:POINTER:FAIL translate\n";
        }
    }
    // Coalescing: moves merge, a wheel or a button change never does.
    let coalesce = supersedes(&state(1, 1, 0, 0), &state(9, 9, 0, 1))
        && supersedes(&state(1, 1, 1, 0), &state(2, 2, 1, 0))
        && !supersedes(&state(1, 1, 0, 1), &state(2, 2, 0, 0))
        && !supersedes(&state(1, 1, 0, 0), &state(1, 1, 1, 0))
        && !supersedes(&state(1, 1, 1, 0), &state(1, 1, 0, 0));
    if !coalesce {
        return "XUID:POINTER:FAIL coalesce\n";
    }
    "XUID:POINTER:PASS\n"
}
