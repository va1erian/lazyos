//! Input policy for `inputd` (`docs/input-plan.md`, layer 2): keymaps,
//! modifier and lock state, key repeat and hotkey matching.
//!
//! Pure `no_std` logic with host tests. It consumes physical key edges (USB
//! HID usages, page 0x07) and produces logical [`Output`]s: key events with a
//! keysym under the active layout, composed text, and hotkey matches. Nothing
//! here knows about Messenger, the kernel bus or the clock: callers pass time
//! in as PIT ticks so the whole state machine is deterministic.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod engine;
pub mod keymap;
pub mod keysym;
mod repeat;
#[cfg(test)]
mod tests;

pub use engine::{Engine, KeyOut, KeyState, Output, RawKey};
pub use keymap::Layout;
pub use repeat::{REPEAT_DELAY_TICKS, REPEAT_INTERVAL_TICKS, TICK_NS};

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
