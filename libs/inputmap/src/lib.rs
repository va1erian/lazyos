//! Input policy for `inputd` (`docs/input-plan.md`, layer 2): keymaps,
//! modifier and lock state, key repeat and hotkey matching, and the pointer
//! (`docs/usb-hid-plan.md`): one cursor and button state for every device;
//! keyboard grabs and the key-state page a focused session polls (I3).
//!
//! Pure `no_std` logic with host tests. It consumes physical key edges (USB
//! HID usages, page 0x07) and produces logical [`Output`]s: key events with a
//! keysym under the active layout, composed text, and hotkey matches. Nothing
//! here knows about Messenger, the kernel bus or the clock: callers pass time
//! in as PIT ticks so the whole state machine is deterministic.

#![cfg_attr(not(test), no_std)]

extern crate alloc;
#[cfg(any(test, feature = "fuzz"))]
extern crate std;

pub mod barrier;
mod engine;
#[cfg(any(test, feature = "fuzz"))]
pub mod fuzz;
pub mod grab;
pub mod hold;
pub mod keymap;
pub mod keystate;
pub mod keysym;
pub mod outbox;
pub mod pointer;
mod repeat;
pub mod router;
#[cfg(test)]
mod tests;

pub use barrier::Barrier;
pub use engine::{Engine, KeyOut, KeyState, Output, RawKey, ESCAPE_CODE, ESCAPE_MODS};
pub use grab::Grabs;
pub use keymap::Layout;
pub use outbox::Outbox;
pub use pointer::{Pointer, PointerOut, RawPointer};
pub use repeat::{REPEAT_DELAY_TICKS, REPEAT_INTERVAL_TICKS, TICK_NS};
pub use router::Router;

/// The `confd` key that selects the keyboard layout (`"us"` or `"fr"`).
pub const LAYOUT_KEY: &str = "sys/input/layout";

/// Modifier and lock bits reported in `KeyEvent.mods` (and matched by hotkeys,
/// which only compare the first four).
pub mod mods {
    pub const SHIFT: u32 = 1 << 0;
    pub const CTRL: u32 = 1 << 1;
    pub const ALT: u32 = 1 << 2;
    /// The Windows/Super key.
    pub const SUPER: u32 = 1 << 3;
    /// AltGr (right Alt on layouts that use it as a level-3 selector).
    pub const ALTGR: u32 = 1 << 4;
    pub const CAPS_LOCK: u32 = 1 << 5;
    pub const NUM_LOCK: u32 = 1 << 6;
    pub const SCROLL_LOCK: u32 = 1 << 7;
    /// The bits a hotkey chord is matched on: the four "command" modifiers.
    pub const CHORD_MASK: u32 = SHIFT | CTRL | ALT | SUPER;
}
