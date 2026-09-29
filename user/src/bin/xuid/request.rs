//! Display protocol request handling (issue #194 split): [`handle_request`]
//! serves `os.lazy.display.v1`, split out of `xuid.rs` unchanged.

use alloc::vec::Vec;
use libmessenger::Parcel;
use user::messenger::display::{self, wire, Canvas, Rect};
use user::messenger::{self, Endpoint, Message};
use user::sys;

use super::anim::deiconify;
use super::drag::{drag_begin, drag_cancel, DragSession};
use super::layout::place_window;
use super::protocol::{
    color_u32, drop_rejected_handle, empty_reply, error_reply, is_privileged, typed_reply,
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
    let body = &message.parcel.body;
    match message.method() {
        wire::METHOD_CREATESURFACE => {
            let Ok(args) = wire::decode_create_surface_args(body) else {
                drop_rejected_handle(message);
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            };
            let (width, height) = (args.width as u64, args.height as u64);
            // An unnamed window still gets a taskbar label.
            let title = if args.title.is_empty() {
                "app".into()
            } else {
                args.title
            };
            let role = args.role;
            // A window can never exceed the screen anyway, and bounding it
            // here keeps `width * height * 4` well inside `i32` downstream
            // (issue #176: an unbounded claim let that multiplication wrap).
            let (max_w, max_h) = (screen.width().max(0) as u64, screen.height().max(0) as u64);
            if width == 0 || height == 0 || width > max_w || height > max_h || message.handles == 0
            {
                drop_rejected_handle(message);
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            }
            if role == wire::ROLE_DESKTOP && !is_privileged(message.sender) {
                // Only an authorized shell identity may own the desktop
                // (issue #175); anyone else's claim is refused outright.
                drop_rejected_handle(message);
                return Some(error_reply(message.method(), messenger::errno::EACCES));
            }
            let id = *next_id;
            *next_id += 1;
            let full = Rect::new(0, 0, screen.width(), screen.height());
            if role == wire::ROLE_DESKTOP {
                // The bottom layer: no chrome, no taskbar entry, never focused.
                // A new desktop replaces the current one.
                if let Some(index) = surfaces.iter().position(|surface| surface.desktop) {
                    let old = surfaces.remove(index);
                    // Tell the old owner and close the endpoint the
                    // compositor held for it (issue #175: both were leaked).
                    let _ = display::send_event(
                        &Endpoint::from_raw(old.events),
                        scratch,
                        wire::METHOD_WINDOWCLOSE,
                        Ok(Vec::new()),
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
                        wire::CHANGE_CREATED,
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
                return Some(typed_reply(
                    message.method(),
                    wire::encode_create_surface_reply(&wire::CreateSurfaceReply { surface: id }),
                ));
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
                    wire::CHANGE_CREATED,
                );
            }
            // Open with a zoom out of the window's taskbar entry, hidden (as if
            // minimized) so the wireframe flies over the old screen.
            if let Some(surface) = surfaces.iter_mut().find(|surface| surface.id == id) {
                surface.minimized = true;
            }
            deiconify(screen, surfaces, pointer, *focused, bar, id);
            if let Some(surface) = surfaces.iter_mut().find(|surface| surface.id == id) {
                surface.minimized = false;
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
            Some(typed_reply(
                message.method(),
                wire::encode_create_surface_reply(&wire::CreateSurfaceReply { surface: id }),
            ))
        }
        wire::METHOD_ATTACHBUFFER => {
            let id = wire::decode_attach_buffer_args(body)
                .unwrap_or_default()
                .surface;
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
        wire::METHOD_COMMIT => {
            let args = wire::decode_commit_args(body).unwrap_or_default();
            let id = args.surface;
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
                    area.x.saturating_add_unsigned(args.x),
                    area.y.saturating_add_unsigned(args.y),
                    i32::try_from(args.w).unwrap_or(i32::MAX),
                    i32::try_from(args.h).unwrap_or(i32::MAX),
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
        wire::METHOD_DESTROYSURFACE => {
            let id = wire::decode_destroy_surface_args(body)
                .unwrap_or_default()
                .surface;
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
        wire::METHOD_DRAGSTART => {
            let args = wire::decode_drag_start_args(body).unwrap_or_default();
            let (id, token, mime) = (args.surface, args.token, args.mime);
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
        wire::METHOD_DRAGCANCEL => {
            let id = wire::decode_drag_cancel_args(body)
                .unwrap_or_default()
                .surface;
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
        wire::METHOD_SUBSCRIBE => {
            let role = wire::decode_subscribe_args(body)
                .unwrap_or_default()
                .subscriber_role;
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
        wire::METHOD_LISTSURFACES => {
            if !is_privileged(message.sender) {
                // Every window title and geometry is compositor-privileged
                // (issue #175); a request from anyone else is refused outright.
                return Some(error_reply(message.method(), messenger::errno::EACCES));
            }
            // One row per surface, in z-order.
            let surfaces = surfaces
                .iter()
                .map(|surface| wire::SurfaceRow {
                    id: surface.id,
                    title: surface.title.clone(),
                    x: surface.x,
                    y: surface.y,
                    w: surface.w,
                    h: surface.h,
                    minimized: surface.minimized,
                    focused: *focused == Some(surface.id),
                    role: surface.role(),
                })
                .collect();
            Some(typed_reply(
                message.method(),
                wire::encode_list_surfaces_reply(&wire::ListSurfacesReply { surfaces }),
            ))
        }
        wire::METHOD_GETWORKAREA => {
            // With a shell registered the fallback bar is hidden, so windows
            // may use the whole screen.
            let height = if taskbar_visible(shell.as_ref()) {
                (screen.height() - TASKBAR_H).max(0)
            } else {
                screen.height()
            };
            Some(typed_reply(
                message.method(),
                wire::encode_get_work_area_reply(&wire::GetWorkAreaReply {
                    x: 0,
                    y: 0,
                    w: screen.width().max(0),
                    h: height,
                }),
            ))
        }
        wire::METHOD_GETTHEME => Some(typed_reply(
            message.method(),
            wire::encode_get_theme_reply(&wire::GetThemeReply {
                title_bg_active: color_u32(TITLE_BG_FOCUS),
                title_bg_inactive: color_u32(TITLE_BG),
                border: color_u32(BORDER_COLOR),
                taskbar: color_u32(TASKBAR_BG),
                text: color_u32(TITLE_TEXT),
            }),
        )),
        _ => Some(error_reply(message.method(), messenger::errno::EINVAL)),
    }
}
