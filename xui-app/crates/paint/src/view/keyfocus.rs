#![forbid(unsafe_code)]

//! Which of the window's own controls has the keyboard.
//!
//! The window's key hook sees every key before the focused widget and cannot
//! ask which widget that is. The canvas, the tool grid, the Colors panel and
//! the Layers panel are the controls that leave plain keys (letters, Delete,
//! the arrows) alone; each one follows its own focus here, so the hook can keep
//! those keys out of a number or text field in the options bar.

use std::cell::Cell;
use std::rc::Rc;

use xui_core::backend::Event;

/// Whether one control has the keyboard; clones share the flag.
#[derive(Clone, Default)]
pub struct KeyOwner(Rc<Cell<bool>>);

impl KeyOwner {
    /// A flag that starts as `focused`.
    pub fn new(focused: bool) -> KeyOwner {
        KeyOwner(Rc::new(Cell::new(focused)))
    }

    /// Follows the focus events the control receives.
    pub fn follow(&self, event: &Event) {
        match event {
            Event::SetFocus => self.0.set(true),
            Event::KillFocus => self.0.set(false),
            _ => {}
        }
    }

    /// Whether the control has the keyboard.
    pub fn get(&self) -> bool {
        self.0.get()
    }
}
