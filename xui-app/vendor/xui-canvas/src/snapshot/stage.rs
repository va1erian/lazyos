#![forbid(unsafe_code)]

//! The [`Stage`] a snapshot's step closure drives.

use xui_core::app::Ui;
use xui_core::backend::{Event, WindowId};
use xui_core::message::{Modifiers, MouseButton};

use crate::OffscreenBackend;

/// The live window a step closure acts on before the capture: it sends
/// messages and injects input, and every action is processed (widgets update,
/// messages reach the app) before it returns.
///
/// Positions are client pixels, as in [`Event`]; at a non-default DPI multiply
/// a design value with [`Dip::to_px`](xui_core::Dip::to_px) and
/// [`Stage::dpi`].
pub struct Stage<'a, M> {
    backend: &'a OffscreenBackend,
    ui: Ui<M>,
}

impl<'a, M: 'static> Stage<'a, M> {
    pub(super) fn new(backend: &'a OffscreenBackend, ui: Ui<M>) -> Stage<'a, M> {
        Stage { backend, ui }
    }

    /// The window handle, for creating more widgets or reading its state.
    pub fn ui(&self) -> &Ui<M> {
        &self.ui
    }

    pub(super) fn window(&self) -> WindowId {
        self.ui.window()
    }

    /// The window's DPI.
    pub fn dpi(&self) -> u32 {
        self.ui.dpi()
    }

    /// Sends `msg` to the app's `update` and processes what it queues.
    pub fn emit(&self, msg: M) {
        self.ui.emit(msg);
        self.backend.pump(self.window());
    }

    /// Delivers `event` to the topmost widget under its position, then lets the
    /// app process the messages it raised. Returns whether a widget consumed it.
    pub fn inject(&self, event: Event) -> bool {
        let consumed = self.backend.inject(self.window(), event);
        self.backend.pump(self.window());
        consumed
    }

    /// Moves the pointer to `(x, y)`, so the widget under it shows its hover
    /// state.
    pub fn hover(&self, x: i32, y: i32) -> bool {
        self.inject(Event::MouseMove {
            x,
            y,
            modifiers: Modifiers::NONE,
        })
    }

    /// Presses and releases the left button at `(x, y)`.
    pub fn click(&self, x: i32, y: i32) -> bool {
        let down = self.inject(Event::MouseDown {
            x,
            y,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        });
        let up = self.inject(Event::MouseUp {
            x,
            y,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        });
        down || up
    }
}
