//! Display protocol request handling (issue #194 split): [`handle_request`]
//! serves `os.lazy.display.v1`, split out of `xuid.rs` unchanged.

use alloc::string::String;
use alloc::vec::Vec;
use libmessenger::{Encoder, Parcel};
use user::messenger::display::{self, Canvas, Rect};
use user::messenger::{self, Endpoint, Message};
use user::sys;

use super::drag::{drag_begin, drag_cancel, DragSession};
use super::layout::place_window;
use super::protocol::{
    color_u64, drop_rejected_handle, empty_reply, error_reply, is_privileged, method, reply_parcel,
    string_field, u64_field,
};
use super::render::repaint;
use super::shell::{
    notify_destroyed, notify_focus, notify_surface, taskbar_visible, AltTab, ShellSub,
};
use super::surface::{Drag, Surface};
use super::theme::{BORDER_COLOR, TASKBAR_BG, TASKBAR_H, TITLE_BG, TITLE_BG_FOCUS, TITLE_TEXT};
use super::window::{remove_surface, surface_by_id, topmost_visible};

/// Handle one display request; returns the reply parcel for a synchronous call.
#[allow(clippy::too_many_arguments)]
pub(super) fn handle_request(
    message: &Message,
    surfaces: &mut Vec<Surface>,
    screen: &mut Canvas,
    next_id: &mut u64,
    focused: &mut Option<u64>,
    drag: &mut Option<Drag>,
    pointer: (i32, i32),
    drag_session: &mut Option<DragSession>,
    scratch: &mut Vec<u8>,
    button_down: bool,
    shell: &mut Option<ShellSub>,
    alt_tab: &mut Option<AltTab>,
) -> Option<Parcel> {
    if message.interface_id() != display::INTERFACE {
        return Some(empty_reply(message.method()));
    }
    let bar = taskbar_visible(shell.as_ref());
    match message.method() {
        method::CREATE_SURFACE => {
            let width = u64_field(&message.parcel, display::field::WIDTH).unwrap_or(0);
            let height = u64_field(&message.parcel, display::field::HEIGHT).unwrap_or(0);
            let title = string_field(&message.parcel, display::field::TITLE)
                .unwrap_or_else(|| String::from("app"));
            let role =
                u64_field(&message.parcel, display::field::ROLE).unwrap_or(display::role::WINDOW);
            // A window can never exceed the screen anyway, and bounding it
            // here keeps `width * height * 4` well inside `i32` downstream
            // (issue #176: an unbounded claim let that multiplication wrap).
            let (max_w, max_h) = (screen.width().max(0) as u64, screen.height().max(0) as u64);
            if width == 0 || height == 0 || width > max_w || height > max_h || message.handles == 0
            {
                drop_rejected_handle(message);
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            }
            if role == display::role::DESKTOP && !is_privileged(message.sender) {
                // Only an authorized shell identity may own the desktop
                // (issue #175); anyone else's claim is refused outright.
                drop_rejected_handle(message);
                return Some(error_reply(message.method(), messenger::errno::EACCES));
            }
            let id = *next_id;
            *next_id += 1;
            let full = Rect::new(0, 0, screen.width(), screen.height());
            if role == display::role::DESKTOP {
                // The bottom layer: no chrome, no taskbar entry, never focused.
                // A new desktop replaces the current one.
                if let Some(index) = surfaces.iter().position(|surface| surface.desktop) {
                    let old = surfaces.remove(index);
                    // Tell the old owner and close the endpoint the
                    // compositor held for it (issue #175: both were leaked).
                    let _ = display::send_event(
                        &Endpoint::from_raw(old.events),
                        scratch,
                        method::WINDOW_CLOSE,
                        0,
                        0,
                    );
                    notify_destroyed(shell.as_ref(), scratch, old.id);
                    let _ = Endpoint::from_raw(old.events).close();
                }
                surfaces.push(Surface {
                    id,
                    title,
                    x: 0,
                    y: 0,
                    w: width as i32,
                    h: height as i32,
                    events: message.first_handle,
                    owner: message.sender,
                    pixels: 0,
                    bytes: 0,
                    minimized: false,
                    desktop: true,
                });
                if let Some(surface) = surface_by_id(surfaces, id) {
                    notify_surface(
                        shell.as_ref(),
                        scratch,
                        surface,
                        *focused,
                        display::change::CREATED,
                    );
                }
                repaint(
                    screen,
                    surfaces,
                    pointer,
                    *focused,
                    full,
                    drag_session.as_ref(),
                    bar,
                    alt_tab.as_ref(),
                );
                let mut body = Encoder::new();
                let _ = body.u64(display::field::SURFACE, id);
                return Some(reply_parcel(message.method(), body));
            }
            let (x, y) = place_window(
                (screen.width(), screen.height()),
                surfaces,
                width as i32,
                height as i32,
            );
            surfaces.push(Surface {
                id,
                title,
                x,
                y,
                w: width as i32,
                h: height as i32,
                events: message.first_handle,
                owner: message.sender,
                pixels: 0,
                bytes: 0,
                minimized: false,
                desktop: false,
            });
            let before = *focused;
            if focused.is_none() {
                *focused = Some(id);
            }
            if *focused != before {
                notify_focus(shell.as_ref(), scratch, *focused);
            }
            if let Some(surface) = surface_by_id(surfaces, id) {
                notify_surface(
                    shell.as_ref(),
                    scratch,
                    surface,
                    *focused,
                    display::change::CREATED,
                );
            }
            // A new surface changes the layout (and the taskbar), so repaint
            // the whole screen.
            repaint(
                screen,
                surfaces,
                pointer,
                *focused,
                full,
                drag_session.as_ref(),
                bar,
                alt_tab.as_ref(),
            );
            let mut body = Encoder::new();
            let _ = body.u64(display::field::SURFACE, id);
            Some(reply_parcel(message.method(), body))
        }
        method::ATTACH_BUFFER => {
            let id = u64_field(&message.parcel, display::field::SURFACE).unwrap_or(0);
            let Some(surface) = surfaces.iter_mut().find(|surface| surface.id == id) else {
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            };
            if surface.owner != message.sender {
                // Only the surface's own client may attach its pixels
                // (issue #176: any caller that guessed the id could spoof
                // another app's window).
                return Some(error_reply(message.method(), messenger::errno::EACCES));
            }
            // The descriptor's length is the sender's claim about how many
            // bytes the surface needs; never trust it to cover the geometry
            // the compositor paints. Checked `u64` arithmetic avoids the
            // wrap a pathological width/height could otherwise cause in the
            // `i32` product (issue #176); `CREATE_SURFACE` also bounds both
            // to the screen size, so this is defense in depth.
            let Some(expected) = (surface.w.max(0) as u64)
                .checked_mul(surface.h.max(0) as u64)
                .and_then(|area| area.checked_mul(4))
            else {
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            };
            let claimed = message
                .parcel
                .buffers
                .first()
                .map(|buffer| buffer.len)
                .unwrap_or(0);
            if message.buffers == 0 || claimed < expected {
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            }
            match sys::display_map_buffer(message.first_buffer) {
                Ok(va) => {
                    surface.pixels = va;
                    surface.bytes = expected;
                    let full = Rect::new(0, 0, screen.width(), screen.height());
                    repaint(
                        screen,
                        surfaces,
                        pointer,
                        *focused,
                        full,
                        drag_session.as_ref(),
                        bar,
                        alt_tab.as_ref(),
                    );
                    Some(empty_reply(message.method()))
                }
                Err(code) => Some(error_reply(message.method(), -code)),
            }
        }
        method::COMMIT => {
            let id = u64_field(&message.parcel, display::field::SURFACE).unwrap_or(0);
            if let Some(surface) = surfaces.iter().find(|surface| surface.id == id) {
                if surface.owner != message.sender {
                    // Only the owner may commit damage (issue #176: any
                    // caller that guessed the id could paint over it).
                    return Some(error_reply(message.method(), messenger::errno::EACCES));
                }
                if surface.minimized {
                    // The pixels are hidden; the minimize repaint already
                    // cleared the screen area. Only the buffer changed.
                    return Some(empty_reply(message.method()));
                }
                // A window's damage is relative to its content origin; the
                // desktop has no chrome, so its origin is the surface origin.
                let area = if surface.desktop {
                    Rect::new(surface.x, surface.y, surface.w, surface.h)
                } else {
                    surface.content()
                };
                let damage = Rect::new(
                    area.x + u64_field(&message.parcel, display::field::X).unwrap_or(0) as i32,
                    area.y + u64_field(&message.parcel, display::field::Y).unwrap_or(0) as i32,
                    u64_field(&message.parcel, display::field::W).unwrap_or(0) as i32,
                    u64_field(&message.parcel, display::field::H).unwrap_or(0) as i32,
                )
                .intersect(area);
                repaint(
                    screen,
                    surfaces,
                    pointer,
                    *focused,
                    damage,
                    drag_session.as_ref(),
                    bar,
                    alt_tab.as_ref(),
                );
            }
            Some(empty_reply(message.method()))
        }
        method::DESTROY_SURFACE => {
            let id = u64_field(&message.parcel, display::field::SURFACE).unwrap_or(0);
            if surface_by_id(surfaces, id).is_some_and(|surface| surface.owner != message.sender) {
                // Only the owner may destroy its own surface (issue #176:
                // any caller that guessed the id could close another app's
                // window).
                return Some(error_reply(message.method(), messenger::errno::EACCES));
            }
            // A window-manager title-bar drag on the surface ends with it.
            if let Some(active) = *drag {
                if active.id == id {
                    *drag = None;
                }
            }
            // A drag & drop session whose source or hovered target goes away
            // ends now.
            let stranding = drag_session
                .as_ref()
                .is_some_and(|active| active.source == id || active.target == Some(id));
            if stranding {
                drag_cancel(
                    drag_session,
                    surfaces,
                    screen,
                    pointer,
                    *focused,
                    scratch,
                    bar,
                    alt_tab.as_ref(),
                );
            }
            notify_destroyed(shell.as_ref(), scratch, id);
            if let Some(tab) = alt_tab.as_mut() {
                // The Alt+Tab snapshot may not outlive the surface.
                tab.order.retain(|&entry| entry != id);
                if tab.order.is_empty() {
                    *alt_tab = None;
                } else if tab.selected >= tab.order.len() {
                    tab.selected = 0;
                }
            }
            remove_surface(surfaces, id);
            if *focused == Some(id) {
                *focused = topmost_visible(surfaces);
                notify_focus(shell.as_ref(), scratch, *focused);
            }
            let full = Rect::new(0, 0, screen.width(), screen.height());
            repaint(
                screen,
                surfaces,
                pointer,
                *focused,
                full,
                drag_session.as_ref(),
                bar,
                alt_tab.as_ref(),
            );
            Some(empty_reply(message.method()))
        }
        method::DRAG_START => {
            let id = u64_field(&message.parcel, display::field::SURFACE).unwrap_or(0);
            let token = u64_field(&message.parcel, display::field::TOKEN).unwrap_or(0);
            let mime = string_field(&message.parcel, display::field::MIME).unwrap_or_default();
            if drag_session.is_some() {
                return Some(error_reply(message.method(), messenger::errno::EBUSY));
            }
            let Some(surface) = surface_by_id(surfaces, id) else {
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            };
            // Only the surface's own client may drag from it, and only with a
            // pointer button held: the gesture is what makes it a drag.
            if surface.owner != message.sender {
                return Some(error_reply(message.method(), messenger::errno::EACCES));
            }
            if token == 0 || mime.is_empty() || mime.len() > display::MAX_MIME || !button_down {
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            }
            drag_begin(
                drag_session,
                surfaces,
                screen,
                pointer,
                *focused,
                scratch,
                bar,
                alt_tab.as_ref(),
                id,
                token,
                mime,
            );
            Some(empty_reply(message.method()))
        }
        method::DRAG_CANCEL => {
            let id = u64_field(&message.parcel, display::field::SURFACE).unwrap_or(0);
            let owns = drag_session
                .as_ref()
                .is_some_and(|active| active.source == id)
                && surface_by_id(surfaces, id).is_some_and(|s| s.owner == message.sender);
            if owns {
                drag_cancel(
                    drag_session,
                    surfaces,
                    screen,
                    pointer,
                    *focused,
                    scratch,
                    bar,
                    alt_tab.as_ref(),
                );
            }
            Some(empty_reply(message.method()))
        }
        method::SUBSCRIBE => {
            let role =
                string_field(&message.parcel, display::field::SUBSCRIBER_ROLE).unwrap_or_default();
            if message.handles == 0 || role.is_empty() || role.len() > display::MAX_ROLE {
                drop_rejected_handle(message);
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            }
            if role == display::ROLE_SHELL && !is_privileged(message.sender) {
                // Only an authorized shell identity may hide the fallback
                // taskbar and receive every surface/focus event (issue
                // #175); anyone else's claim is refused outright.
                drop_rejected_handle(message);
                return Some(error_reply(message.method(), messenger::errno::EACCES));
            }
            // One subscriber at a time; a re-subscribe replaces the
            // endpoint, so close the one it replaces (issue #175: it was
            // leaked).
            let previous = shell.replace(ShellSub {
                role,
                events: message.first_handle,
            });
            if let Some(previous) = previous {
                let _ = Endpoint::from_raw(previous.events).close();
            }
            let full = Rect::new(0, 0, screen.width(), screen.height());
            repaint(
                screen,
                surfaces,
                pointer,
                *focused,
                full,
                drag_session.as_ref(),
                taskbar_visible(shell.as_ref()),
                alt_tab.as_ref(),
            );
            Some(empty_reply(message.method()))
        }
        method::LIST_SURFACES => {
            if !is_privileged(message.sender) {
                // Every window's title and geometry is compositor-privileged
                // (issue #175); anyone else's request is refused outright.
                return Some(error_reply(message.method(), messenger::errno::EACCES));
            }
            let mut body = Encoder::new();
            // One row per surface, in z-order: `SURFACE` starts a row and the
            // trailing fields describe it.
            for surface in surfaces.iter() {
                let role = if surface.desktop {
                    display::role::DESKTOP
                } else {
                    display::role::WINDOW
                };
                let _ = body.u64(display::field::SURFACE, surface.id);
                let _ = body.string(display::field::TITLE, &surface.title);
                let _ = body.u64(display::field::X, surface.x.max(0) as u64);
                let _ = body.u64(display::field::Y, surface.y.max(0) as u64);
                let _ = body.u64(display::field::W, surface.w.max(0) as u64);
                let _ = body.u64(display::field::H, surface.h.max(0) as u64);
                let _ = body.u64(display::field::MINIMIZED, surface.minimized as u64);
                let _ = body.u64(
                    display::field::FOCUSED,
                    (*focused == Some(surface.id)) as u64,
                );
                let _ = body.u64(display::field::ROLE, role);
            }
            Some(reply_parcel(message.method(), body))
        }
        method::GET_WORK_AREA => {
            // With a shell registered the fallback bar is hidden, so windows
            // may use the whole screen.
            let height = if taskbar_visible(shell.as_ref()) {
                (screen.height() - TASKBAR_H).max(0)
            } else {
                screen.height()
            };
            let mut body = Encoder::new();
            let _ = body.u64(display::field::X, 0);
            let _ = body.u64(display::field::Y, 0);
            let _ = body.u64(display::field::W, screen.width().max(0) as u64);
            let _ = body.u64(display::field::H, height as u64);
            Some(reply_parcel(message.method(), body))
        }
        method::GET_THEME => {
            let mut body = Encoder::new();
            let _ = body.u64(display::field::TITLE_BG_ACTIVE, color_u64(TITLE_BG_FOCUS));
            let _ = body.u64(display::field::TITLE_BG_INACTIVE, color_u64(TITLE_BG));
            let _ = body.u64(display::field::BORDER, color_u64(BORDER_COLOR));
            let _ = body.u64(display::field::TASKBAR, color_u64(TASKBAR_BG));
            let _ = body.u64(display::field::TEXT, color_u64(TITLE_TEXT));
            Some(reply_parcel(message.method(), body))
        }
        _ => Some(error_reply(message.method(), messenger::errno::EINVAL)),
    }
}
