//! Window-management operations (issues #143, #175, #194 split): z-order,
//! focus, minimize/close, lookup and hit helpers, and per-surface event
//! forwarding, moved out of `xuid.rs` unchanged.

use alloc::vec::Vec;
use user::messenger::display::{self, Canvas, Rect};
use user::messenger::Endpoint;

use super::drag::DragSession;
use super::protocol::method;
use super::render::repaint;
use super::shell::{notify_destroyed, notify_focus, notify_surface, AltTab, ShellSub};
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
pub(super) fn relative(surfaces: &[Surface], id: u64, point: (i32, i32)) -> (i64, i64) {
    match surfaces.iter().find(|surface| surface.id == id) {
        Some(surface) => {
            let content = surface.content();
            ((point.0 - content.x) as i64, (point.1 - content.y) as i64)
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

/// Minimize a surface, moving focus to the next visible surface.
#[allow(clippy::too_many_arguments)]
pub(super) fn minimize_surface(
    surfaces: &mut [Surface],
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: &mut Option<u64>,
    drag_session: Option<&DragSession>,
    shell: Option<&ShellSub>,
    scratch: &mut Vec<u8>,
    taskbar: bool,
    alt_tab: Option<&AltTab>,
    id: u64,
) {
    if let Some(surface) = surfaces.iter_mut().find(|surface| surface.id == id) {
        surface.minimized = true;
    }
    if *focused == Some(id) {
        *focused = topmost_visible(surfaces);
        notify_focus(shell, scratch, *focused);
    }
    // The row carries the post-minimize focus flag, so send it after the focus
    // recompute.
    if let Some(surface) = surface_by_id(surfaces, id) {
        notify_surface(
            shell,
            scratch,
            surface,
            *focused,
            display::change::MINIMIZED,
        );
    }
    let full = Rect::new(0, 0, screen.width(), screen.height());
    repaint(
        screen,
        surfaces,
        pointer,
        *focused,
        full,
        drag_session,
        taskbar,
        alt_tab,
    );
}

/// Close a surface: tell the client through a one-way `WindowClose` event and
/// drop it; the full-screen repaint lets the windows below show through.
#[allow(clippy::too_many_arguments)]
pub(super) fn close_surface(
    surfaces: &mut Vec<Surface>,
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: &mut Option<u64>,
    scratch: &mut Vec<u8>,
    drag_session: Option<&DragSession>,
    shell: Option<&ShellSub>,
    taskbar: bool,
    alt_tab: Option<&AltTab>,
    id: u64,
) {
    if let Some(surface) = surfaces.iter().find(|surface| surface.id == id) {
        let _ = display::send_event(
            &Endpoint::from_raw(surface.events),
            scratch,
            method::WINDOW_CLOSE,
            0,
            0,
        );
    }
    notify_destroyed(shell, scratch, id);
    remove_surface(surfaces, id);
    if *focused == Some(id) {
        *focused = topmost_visible(surfaces);
        notify_focus(shell, scratch, *focused);
    }
    let full = Rect::new(0, 0, screen.width(), screen.height());
    repaint(
        screen,
        surfaces,
        pointer,
        *focused,
        full,
        drag_session,
        taskbar,
        alt_tab,
    );
}

/// Drop surface `id` and close its transferred event endpoint, so repeated
/// create/destroy cycles cannot exhaust this task's handle table. Events
/// already queued (e.g. `WindowClose`) stay deliverable after the close.
pub(super) fn remove_surface(surfaces: &mut Vec<Surface>, id: u64) {
    if let Some(index) = surfaces.iter().position(|surface| surface.id == id) {
        let surface = surfaces.remove(index);
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
/// Send one event to a surface's endpoint, ignoring a closed peer.
pub(super) fn forward(
    surfaces: &[Surface],
    scratch: &mut Vec<u8>,
    id: Option<u64>,
    method: u32,
    a: i64,
    b: i64,
) {
    let Some(surface) = id.and_then(|id| surfaces.iter().find(|surface| surface.id == id)) else {
        return;
    };
    let _ = display::send_event(
        &Endpoint::from_raw(surface.events),
        scratch,
        method,
        a as u64,
        b as u64,
    );
}
