//! The messages the Terminal's window and timers send it.

use std::path::PathBuf;

use xui_core::{Key, Modifiers};

use super::pointer::MouseMsg;

/// One application message.
pub(super) enum Msg {
    /// The poll timer fired: drain output, maybe finish.
    Tick,
    /// A drag past the edge of the grid scrolls on this timer.
    Autoscroll,
    /// Input the pty had no room for is waiting: try again.
    Flush,
    /// A translated character from the keyboard.
    Char(char),
    /// A key press with the modifiers held: arrows, Escape and the like, and
    /// Ctrl+letter chords (which arrive without a `Char`). Enter and
    /// Backspace also arrive as `Char`s, which is the path that handles them.
    Key(Key, Modifiers),
    /// A wheel roll (`delta` is 120 a notch, positive up) at node pixel
    /// (`x`, `y`) with the modifiers held.
    Wheel(i16, Modifiers, i32, i32),
    /// A pointer press, release, move or double-click.
    Mouse(MouseMsg),
    /// Files dropped on the window.
    Dropped(Vec<PathBuf>),
    Close,
}
