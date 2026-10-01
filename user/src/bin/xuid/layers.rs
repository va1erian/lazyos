//! Pointer routing to the shell's chromeless layers (issue #157): the desktop
//! below every window and the panels (taskbar, start menu) above them.
//!
//! A layer gets the pointer while it is over it: hover moves too, and one last
//! `PointerMove` at `(-1, -1)` (outside any surface) when the pointer leaves,
//! so hover highlights clear. A press on a layer grabs the pointer to it until
//! every button is released, like a window's content press keeps the window's
//! moves. A press on a panel never changes window focus; any other press tells
//! the shell to `Dismiss` its popups.

use user::messenger::display::wire;

use super::compositor::Compositor;
use super::window::{contains, forward, relative};

/// Where the leave move lands: outside every surface's coordinates.
const LEFT_AT: (i32, i32) = (-1, -1);

impl Compositor {
    /// The topmost panel under `point`.
    pub(super) fn panel_at(&self, point: (i32, i32)) -> Option<u64> {
        self.surfaces
            .iter()
            .rev()
            .find(|surface| surface.is_panel() && contains(surface.window(), point))
            .map(|surface| surface.id)
    }

    /// The desktop, when it is what shows at `point` (no window covers it).
    pub(super) fn desktop_at(&self, point: (i32, i32)) -> Option<u64> {
        let covered = self.surfaces.iter().any(|surface| {
            surface.is_window() && !surface.minimized && contains(surface.window(), point)
        });
        if covered {
            return None;
        }
        self.surfaces
            .iter()
            .find(|surface| surface.is_desktop() && contains(surface.window(), point))
            .map(|surface| surface.id)
    }

    /// Send layer `id` a pointer event of `method` at the screen `point`
    /// (or at [`LEFT_AT`] when `None`), with `button` for a press or release.
    fn send_layer(&mut self, id: u64, method: u32, point: Option<(i32, i32)>, button: u32) {
        let (x, y) = point.map_or(LEFT_AT, |point| relative(&self.surfaces, id, point));
        let body = match method {
            wire::METHOD_POINTERDOWN => {
                wire::encode_pointer_down_args(&wire::PointerDownArgs { x, y, button })
            }
            wire::METHOD_POINTERUP => {
                wire::encode_pointer_up_args(&wire::PointerUpArgs { x, y, button })
            }
            _ => wire::encode_pointer_move_args(&wire::PointerMoveArgs { x, y }),
        };
        forward(&self.surfaces, &mut self.scratch, Some(id), method, body);
    }

    /// Route a move to the grabbing or hovered layer. `true` when a grab
    /// owns the pointer, so the focused window must not see the move.
    pub(super) fn layer_move(&mut self, point: (i32, i32)) -> bool {
        if let Some((id, _)) = self.grab {
            self.send_layer(id, wire::METHOD_POINTERMOVE, Some(point), 0);
            return true;
        }
        let target = self.panel_at(point).or_else(|| self.desktop_at(point));
        if self.hover != target {
            if let Some(left) = self.hover {
                self.send_layer(left, wire::METHOD_POINTERMOVE, None, 0);
            }
            self.hover = target;
        }
        if let Some(id) = target {
            self.send_layer(id, wire::METHOD_POINTERMOVE, Some(point), 0);
        }
        false
    }

    /// A press while a layer holds the grab, or on a panel: deliver it and
    /// grab. `true` when it was a layer's.
    pub(super) fn layer_down(&mut self, button: u32) -> bool {
        let point = self.pointer;
        let Some(id) = self.grab.map(|(id, _)| id).or_else(|| self.panel_at(point)) else {
            return false;
        };
        self.grab_layer(id, button);
        true
    }

    /// A press on the bare desktop: deliver it and grab. `false` when no
    /// desktop shows at the pointer.
    pub(super) fn desktop_down(&mut self, button: u32) -> bool {
        let Some(id) = self.desktop_at(self.pointer) else {
            return false;
        };
        self.grab_layer(id, button);
        true
    }

    /// Add `button` to layer `id`'s grab and send it the press.
    fn grab_layer(&mut self, id: u64, button: u32) {
        let held = self.grab.map_or(0, |(_, held)| held);
        self.grab = Some((id, held | 1 << (button & 31)));
        self.hover = Some(id);
        let point = self.pointer;
        self.send_layer(id, wire::METHOD_POINTERDOWN, Some(point), button);
    }

    /// A release while a layer holds the grab: deliver it, and end the grab
    /// with the last held button. `true` when it was the layer's.
    pub(super) fn layer_up(&mut self, button: u32) -> bool {
        let Some((id, held)) = self.grab else {
            return false;
        };
        let held = held & !(1 << (button & 31));
        self.grab = (held != 0).then_some((id, held));
        let point = self.pointer;
        self.send_layer(id, wire::METHOD_POINTERUP, Some(point), button);
        true
    }

    /// Forget surface `id` as the pointer's grab or hover target.
    pub(super) fn forget_layer(&mut self, id: u64) {
        if self.grab.is_some_and(|(grabbed, _)| grabbed == id) {
            self.grab = None;
        }
        if self.hover == Some(id) {
            self.hover = None;
        }
    }
}
