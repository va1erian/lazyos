//! Display protocol request handling (issue #194 split):
//! [`Compositor::handle_request`] serves `os.lazy.display.v1`. Surface
//! lifecycle requests live here; drag & drop and the shell queries live in
//! `request_shell.rs`.

use alloc::vec::Vec;
use libmessenger::Parcel;
use user::messenger::display::{self, wire, Rect};
use user::messenger::{self, Endpoint, Message};

use super::compositor::Compositor;
use super::geometry::{self, SizeHints};
use super::layout::place_window;
use super::present::attach;
use super::protocol::{drop_rejected_handle, empty_reply, error_reply, typed_reply};
use super::surface::Surface;
use super::theme::{BORDER, TITLE_H};
use super::window::{focus_on_create, surface_by_id};

/// The most panels that may exist at once: a taskbar and a few popups need
/// far fewer, and each one costs every repaint an occlusion test.
const MAX_PANELS: usize = 16;

impl Compositor {
    /// Handle one display request; returns the reply parcel for a synchronous
    /// call.
    pub(super) fn handle_request(&mut self, message: &Message) -> Parcel {
        if message.interface_id() != display::INTERFACE {
            return empty_reply(message.method());
        }
        let body = &message.parcel.body;
        // Every window's title, geometry and state is the shell's (issues
        // #175, #157): anyone else is refused before decoding anything.
        let shell_only = matches!(
            message.method(),
            wire::METHOD_LISTSURFACES
                | wire::METHOD_ACTIVATESURFACE
                | wire::METHOD_MINIMIZESURFACE
                | wire::METHOD_SETWORKAREA
                | wire::METHOD_SETICONGEOMETRY
                | wire::METHOD_HINTLAUNCHORIGIN
        );
        if shell_only && !self.is_shell_caller(message.sender) {
            return error_reply(message.method(), messenger::errno::EACCES);
        }
        match message.method() {
            wire::METHOD_CREATESURFACE => self.create_surface(message, body),
            wire::METHOD_ATTACHBUFFER => self.attach_buffer(message, body),
            wire::METHOD_ATTACHBUFFERSLOT => self.attach_buffer_slot(message, body),
            wire::METHOD_COMMIT => self.commit(message, body),
            wire::METHOD_DESTROYSURFACE => self.destroy_surface(message, body),
            wire::METHOD_DRAGSTART => self.drag_start(message, body),
            wire::METHOD_DRAGCANCEL => self.drag_cancel_request(message, body),
            wire::METHOD_SETTITLE => self.set_title(message, body),
            wire::METHOD_HINTOPENORIGIN => self.hint_open_origin(message, body),
            wire::METHOD_SETSIZEHINTS => self.set_size_hints(message, body),
            wire::METHOD_REQUESTSIZE => self.request_size(message, body),
            wire::METHOD_SUBSCRIBE => self.subscribe(message, body),
            wire::METHOD_LISTSURFACES => self.list_surfaces(message),
            wire::METHOD_GETWORKAREA => self.get_work_area(message),
            wire::METHOD_GETTHEME => super::request_shell::get_theme(message),
            wire::METHOD_PLACESURFACE => self.place_surface(message, body),
            wire::METHOD_ACTIVATESURFACE => self.activate_surface(message, body),
            wire::METHOD_MINIMIZESURFACE => self.minimize_request(message, body),
            wire::METHOD_SETWORKAREA => self.set_work_area(message, body),
            wire::METHOD_SETICONGEOMETRY => self.set_icon_geometry(message, body),
            wire::METHOD_HINTLAUNCHORIGIN => self.hint_launch_origin(message, body),
            _ => error_reply(message.method(), messenger::errno::EINVAL),
        }
    }

    /// `CreateSurface`: a window, or (shell-only) the desktop or a panel.
    fn create_surface(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let Ok(args) = wire::decode_create_surface_args(body) else {
            drop_rejected_handle(message);
            return error_reply(message.method(), messenger::errno::EINVAL);
        };
        let (width, height) = (args.width as u64, args.height as u64);
        // An unnamed window still gets a taskbar label.
        let title = if args.title.is_empty() {
            "app".into()
        } else {
            args.title
        };
        let role = args.role;
        // A window can never exceed the screen anyway, and bounding it here
        // keeps `width * height * 4` well inside `i32` downstream (issue
        // #176: an unbounded claim let that multiplication wrap).
        let (max_w, max_h) = (
            self.screen.width().max(0) as u64,
            self.screen.height().max(0) as u64,
        );
        if width == 0
            || height == 0
            || width > max_w
            || height > max_h
            || !message.carries(wire::CREATE_SURFACE_TRANSFERS)
        {
            drop_rejected_handle(message);
            return error_reply(message.method(), messenger::errno::EINVAL);
        }
        let panels = self.surfaces.iter().filter(|s| s.is_panel()).count();
        let refusal = match role {
            wire::ROLE_WINDOW => None,
            // Only the shell may own the desktop or a panel (issues #175,
            // #157); anyone else's claim is refused outright.
            wire::ROLE_DESKTOP | wire::ROLE_PANEL if !self.is_shell_caller(message.sender) => {
                Some(messenger::errno::EACCES)
            }
            wire::ROLE_PANEL if panels >= MAX_PANELS => Some(messenger::errno::EBUSY),
            wire::ROLE_DESKTOP | wire::ROLE_PANEL => None,
            _ => Some(messenger::errno::EINVAL),
        };
        if let Some(code) = refusal {
            drop_rejected_handle(message);
            return error_reply(message.method(), code);
        }
        let id = self.next_id;
        self.next_id += 1;
        let (w, h) = (width as i32, height as i32);
        if role != wire::ROLE_WINDOW {
            // A chromeless layer at (0, 0): the desktop below every window
            // (a new one replaces the current one), or a panel above them,
            // which the shell moves with `PlaceSurface`. Neither is focused
            // or listed in Alt+Tab.
            if role == wire::ROLE_DESKTOP {
                self.replace_desktop();
            }
            self.surfaces
                .push(new_surface(message, id, title, (0, 0), (w, h), role));
            self.notify_surface(id, wire::CHANGE_CREATED);
            self.repaint(Rect::new(0, 0, w, h));
        } else {
            let origin = place_window(self.work_area(), &self.surfaces, w, h);
            self.surfaces
                .push(new_surface(message, id, title, origin, (w, h), role));
            // A new window comes up on top and focused, so double-clicking a
            // folder in Files shows the new window in front instead of behind
            // the one that opened it. The first window still gets focus.
            if focus_on_create(&mut self.surfaces, &mut self.focused, id) {
                self.notify_focus();
            }
            self.notify_surface(id, wire::CHANGE_CREATED);
            // Open with a zoom out of the tile the app (or the shell) hinted
            // at, else out of the window's icon; hidden (as if minimized) so
            // the wireframe flies over the old screen.
            let origin = self.take_open_origin(message.sender);
            self.set_minimized(id, true);
            self.open_zoom(id, origin);
            self.set_minimized(id, false);
            self.repaint_full();
        }
        // Register the surface with `inputd` before the client learns its id,
        // so the `Open` it sends next can find it.
        self.sync_input();
        typed_reply(
            message.method(),
            wire::encode_create_surface_reply(&wire::CreateSurfaceReply { surface: id }),
        )
    }

    /// Drop the current desktop surface, if any: tell its owner and close the
    /// endpoint the compositor held for it (issue #175: both were leaked).
    fn replace_desktop(&mut self) {
        let Some(index) = self.surfaces.iter().position(Surface::is_desktop) else {
            return;
        };
        let mut old = self.surfaces.remove(index);
        old.release_buffers();
        let _ = display::send_event(
            &Endpoint::from_raw(old.events),
            &mut self.scratch,
            wire::METHOD_WINDOWCLOSE,
            Ok(Vec::new()),
        );
        self.notify_destroyed(old.id);
        let _ = Endpoint::from_raw(old.events).close();
    }

    /// `AttachBuffer`: map the client's pixel buffer as slot 0 and show it.
    fn attach_buffer(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let id = wire::decode_attach_buffer_args(body)
            .unwrap_or_default()
            .surface;
        match attach(message, &mut self.surfaces, id, None) {
            Ok(()) => {
                self.repaint_full();
                empty_reply(message.method())
            }
            Err(code) => error_reply(message.method(), code),
        }
    }

    /// `AttachBufferSlot` (issue #361): register one buffer slot. It is not
    /// the current slot, so nothing on screen changes.
    fn attach_buffer_slot(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let args = wire::decode_attach_buffer_slot_args(body).unwrap_or_default();
        match attach(message, &mut self.surfaces, args.surface, Some(args.slot)) {
            Ok(()) => empty_reply(message.method()),
            Err(code) => error_reply(message.method(), code),
        }
    }

    /// `Commit`: the client's damaged rectangle is ready; repaint it.
    fn commit(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let args = wire::decode_commit_args(body).unwrap_or_default();
        let id = args.surface;
        let Some(surface) = self.surfaces.iter().find(|surface| surface.id == id) else {
            return empty_reply(message.method());
        };
        if surface.owner != message.sender {
            // Only the owner may commit damage (issue #176: any caller that
            // guessed the id could paint over it).
            return error_reply(message.method(), messenger::errno::EACCES);
        }
        if surface.minimized {
            // The pixels are hidden; the minimize repaint already cleared the
            // screen area. Only the buffer changed.
            return empty_reply(message.method());
        }
        // Damage is relative to the content origin (a chromeless surface's
        // content is all of it).
        let area = surface.content();
        let damage = Rect::new(
            area.x.saturating_add_unsigned(args.x),
            area.y.saturating_add_unsigned(args.y),
            i32::try_from(args.w).unwrap_or(i32::MAX),
            i32::try_from(args.h).unwrap_or(i32::MAX),
        )
        .intersect(area);
        self.repaint(damage);
        empty_reply(message.method())
    }

    /// `SetSizeHints`: declare `surface` resizable within content-size bounds.
    /// Only the owner may call it; a bad bound is `EINVAL`, an unknown surface
    /// `ENOENT`. A fixed-size window stays fixed, so old clients are
    /// unaffected.
    fn set_size_hints(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let Ok(args) = wire::decode_set_size_hints_args(body) else {
            return error_reply(message.method(), messenger::errno::EINVAL);
        };
        let Some(surface) = surface_by_id(&self.surfaces, args.surface) else {
            return error_reply(message.method(), messenger::errno::ENOENT);
        };
        if surface.owner != message.sender {
            return error_reply(message.method(), messenger::errno::EACCES);
        }
        // The desktop and panels have no chrome to resize.
        if !surface.is_window() {
            return error_reply(message.method(), messenger::errno::EINVAL);
        }
        let Some(hints) = SizeHints::new(
            args.min_w,
            args.min_h,
            args.max_w,
            args.max_h,
            (self.screen.width(), self.screen.height()),
        ) else {
            return error_reply(message.method(), messenger::errno::EINVAL);
        };
        if let Some(surface) = self.surfaces.iter_mut().find(|s| s.id == args.surface) {
            surface.hints = Some(hints);
        }
        empty_reply(message.method())
    }

    /// `RequestSize`: the owner asks for a new content size (a compact or
    /// expanded view). Needs declared size hints and a normal (neither
    /// maximized nor minimized) window; the size is clamped by
    /// [`geometry::requested_rect`] and the client is always answered with a
    /// `Configure`, even when nothing changed, so it never waits in vain.
    fn request_size(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let Ok(args) = wire::decode_request_size_args(body) else {
            return error_reply(message.method(), messenger::errno::EINVAL);
        };
        let Some(surface) = surface_by_id(&self.surfaces, args.surface) else {
            return error_reply(message.method(), messenger::errno::ENOENT);
        };
        if surface.owner != message.sender {
            return error_reply(message.method(), messenger::errno::EACCES);
        }
        let Some(hints) = surface.hints.filter(|_| surface.resizable()) else {
            return error_reply(message.method(), messenger::errno::EINVAL);
        };
        if surface.minimized || surface.maximized.is_some() {
            return error_reply(message.method(), messenger::errno::EINVAL);
        }
        let (window, events) = (surface.window(), surface.events);
        let rect =
            geometry::requested_rect(window, (args.width, args.height), &hints, self.work_area());
        if rect == window {
            self.send_configure(
                args.surface,
                events,
                window.w - BORDER * 2,
                window.h - TITLE_H - BORDER,
                wire::WINDOW_STATE_NORMAL,
            );
            return empty_reply(message.method());
        }
        if let Some(surface) = self.surfaces.iter_mut().find(|s| s.id == args.surface) {
            surface.x = rect.x;
            surface.y = rect.y;
            surface.w = rect.w - BORDER * 2;
            surface.h = rect.h - TITLE_H - BORDER;
        }
        self.send_configure(
            args.surface,
            events,
            rect.w - BORDER * 2,
            rect.h - TITLE_H - BORDER,
            wire::WINDOW_STATE_NORMAL,
        );
        self.notify_surface(args.surface, wire::CHANGE_RESIZED);
        // The old footprint may be larger than the new one: repaint it all.
        self.repaint_full();
        empty_reply(message.method())
    }

    /// `DestroySurface`: only the owner may close its window.
    fn destroy_surface(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let id = wire::decode_destroy_surface_args(body)
            .unwrap_or_default()
            .surface;
        if surface_by_id(&self.surfaces, id).is_some_and(|surface| surface.owner != message.sender)
        {
            // Only the owner may destroy its own surface (issue #176: any
            // caller that guessed the id could close another app's window).
            return error_reply(message.method(), messenger::errno::EACCES);
        }
        self.forget_surface(id);
        self.repaint_full();
        empty_reply(message.method())
    }
}

/// A freshly created, unattached surface for `message`'s sender.
fn new_surface(
    message: &Message,
    id: u64,
    title: alloc::string::String,
    origin: (i32, i32),
    size: (i32, i32),
    role: u32,
) -> Surface {
    Surface {
        id,
        title,
        x: origin.0,
        y: origin.1,
        w: size.0,
        h: size.1,
        events: message.first_handle,
        owner: message.sender,
        pixels: 0,
        bytes: 0,
        buf_w: 0,
        buf_h: 0,
        hints: None,
        maximized: None,
        minimized: false,
        role,
        icon: None,
        input_session: false,
        slots: Default::default(),
    }
}
