//! Window-management operations (issues #143, #175, #194 split): z-order,
//! focus, minimize/close, lookup and hit helpers, and per-surface event
//! forwarding, moved out of `xuid.rs` unchanged.

use alloc::vec::Vec;
use user::messenger::display::{self, wire, Rect};
use user::messenger::Endpoint;

use super::compositor::Compositor;
use super::surface::Surface;

/// Find a surface by id.
pub(super) fn surface_by_id(surfaces: &[Surface], id: u64) -> Option<&Surface> {
    surfaces.iter().find(|surface| surface.id == id)
}
/// Whether `rect` contains the point `(x, y)`.
pub(super) fn contains(rect: Rect, point: (i32, i32)) -> bool {
    point.0 >= rect.x && point.1 >= rect.y && point.0 < rect.x + rect.w && point.1 < rect.y + rect.h
}

/// The surface-relative pointer position inside a window's content.
pub(super) fn relative(surfaces: &[Surface], id: u64, point: (i32, i32)) -> (i32, i32) {
    match surfaces.iter().find(|surface| surface.id == id) {
        Some(surface) => {
            let content = surface.content();
            (point.0 - content.x, point.1 - content.y)
        }
        None => (0, 0),
    }
}

/// Move a surface to the tail of `surfaces`, i.e. the top of the paint order.
pub(super) fn raise(surfaces: &mut Vec<Surface>, id: u64) {
    if let Some(index) = surfaces.iter().position(|surface| surface.id == id) {
        if index + 1 != surfaces.len() {
            let surface = surfaces.remove(index);
            surfaces.push(surface);
        }
    }
}

/// The topmost visible window's id (desktops are never focusable).
pub(super) fn topmost_visible(surfaces: &[Surface]) -> Option<u64> {
    surfaces
        .iter()
        .rev()
        .find(|surface| !surface.desktop && !surface.minimized)
        .map(|surface| surface.id)
}

/// Focus a taskbar entry: restore it if minimized, raise it, and focus it.
pub(super) fn restore(surfaces: &mut Vec<Surface>, focused: &mut Option<u64>, id: u64) {
    if let Some(surface) = surfaces.iter_mut().find(|surface| surface.id == id) {
        surface.minimized = false;
    }
    raise(surfaces, id);
    *focused = Some(id);
}

impl Compositor {
    /// Minimize a surface, moving focus to the next visible surface.
    pub(super) fn minimize_surface(&mut self, id: u64) {
        if let Some(surface) = self.surfaces.iter_mut().find(|surface| surface.id == id) {
            surface.minimized = true;
        }
        if self.focused == Some(id) {
            self.focused = topmost_visible(&self.surfaces);
            self.notify_focus();
        }
        // The row carries the post-minimize focus flag, so send it after the
        // focus recompute.
        self.notify_surface(id, wire::CHANGE_MINIMIZED);
        self.repaint_full();
    }

    /// Close a surface: tell the client through a one-way `WindowClose` event
    /// and drop it; the full-screen repaint lets the windows below show
    /// through.
    pub(super) fn close_surface(&mut self, id: u64) {
        if let Some(surface) = self.surfaces.iter().find(|surface| surface.id == id) {
            let _ = display::send_event(
                &Endpoint::from_raw(surface.events),
                &mut self.scratch,
                wire::METHOD_WINDOWCLOSE,
                Ok(Vec::new()),
            );
        }
        self.forget_surface(id);
        self.repaint_full();
    }

    /// Drop every trace of surface `id`: the title-bar drag, a drag & drop
    /// session it sources or hovers, its Alt+Tab entry, the shell's view of
    /// it and its focus. Shared by close and destroy so neither can leave a
    /// dangling id behind; the caller repaints.
    pub(super) fn forget_surface(&mut self, id: u64) {
        // A window-manager title-bar drag on the surface ends with it.
        if self.drag.is_some_and(|active| active.id == id) {
            self.drag = None;
        }
        // A drag & drop session whose source or hovered target goes away ends
        // now.
        let stranding = self
            .drag_session
            .as_ref()
            .is_some_and(|active| active.source == id || active.target == Some(id));
        if stranding {
            self.drag_cancel();
        }
        self.notify_destroyed(id);
        if let Some(tab) = self.alt_tab.as_mut() {
            // The Alt+Tab snapshot may not outlive the surface.
            tab.order.retain(|&entry| entry != id);
            if tab.order.is_empty() {
                self.alt_tab = None;
            } else if tab.selected >= tab.order.len() {
                tab.selected = 0;
            }
        }
        remove_surface(&mut self.surfaces, id);
        if self.focused == Some(id) {
            self.focused = topmost_visible(&self.surfaces);
            self.notify_focus();
        }
    }
}

/// Drop surface `id` and close its transferred event endpoint, so repeated
/// create/destroy cycles cannot exhaust this task's handle table. Events
/// already queued (e.g. `WindowClose`) stay deliverable after the close.
pub(super) fn remove_surface(surfaces: &mut Vec<Surface>, id: u64) {
    if let Some(index) = surfaces.iter().position(|surface| surface.id == id) {
        let mut surface = surfaces.remove(index);
        surface.release_buffers();
        if surface.events != 0 {
            let _ = Endpoint::from_raw(surface.events).close();
        }
    }
}

/// Move focus to the next visible surface, wrapping around and skipping
/// minimized ones; the new focus is raised so its title bar is not covered.
pub(super) fn cycle_focus(surfaces: &mut Vec<Surface>, focused: &mut Option<u64>) {
    if surfaces
        .iter()
        .filter(|surface| !surface.desktop)
        .all(|surface| surface.minimized)
    {
        *focused = None;
        return;
    }
    let current_id = *focused;
    if let Some(current) = current_id.and_then(|id| surfaces.iter().position(|s| s.id == id)) {
        for step in 1..=surfaces.len() {
            let index = (current + step) % surfaces.len();
            if !surfaces[index].desktop
                && !surfaces[index].minimized
                && Some(surfaces[index].id) != current_id
            {
                let id = surfaces[index].id;
                raise(surfaces, id);
                *focused = Some(id);
                return;
            }
        }
    }
    // No other visible surface: focus (and raise) the first visible one.
    if let Some(id) = surfaces
        .iter()
        .find(|surface| !surface.desktop && !surface.minimized)
        .map(|surface| surface.id)
    {
        raise(surfaces, id);
        *focused = Some(id);
    }
}
/// Send one event (`body` already encoded) to a surface's endpoint, ignoring
/// a closed peer.
pub(super) fn forward(
    surfaces: &[Surface],
    scratch: &mut Vec<u8>,
    id: Option<u64>,
    method: u32,
    body: Result<Vec<u8>, libmessenger::Error>,
) {
    let Some(surface) = id.and_then(|id| surfaces.iter().find(|surface| surface.id == id)) else {
        return;
    };
    let _ = display::send_event(&Endpoint::from_raw(surface.events), scratch, method, body);
}
