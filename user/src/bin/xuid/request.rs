//! Display protocol request handling (issue #194 split):
//! [`Compositor::handle_request`] serves `os.lazy.display.v1`. Surface
//! lifecycle requests live here; drag & drop and the shell queries live in
//! `request_shell.rs`.

use alloc::vec::Vec;
use libmessenger::Parcel;
use user::messenger::display::{self, wire, Rect};
use user::messenger::{self, Endpoint, Message};

use super::compositor::Compositor;
use super::layout::place_window;
use super::present::attach;
use super::protocol::{drop_rejected_handle, empty_reply, error_reply, is_privileged, typed_reply};
use super::surface::Surface;
use super::window::surface_by_id;

impl Compositor {
    /// Handle one display request; returns the reply parcel for a synchronous
    /// call.
    pub(super) fn handle_request(&mut self, message: &Message) -> Parcel {
        if message.interface_id() != display::INTERFACE {
            return empty_reply(message.method());
        }
        let body = &message.parcel.body;
        match message.method() {
            wire::METHOD_CREATESURFACE => self.create_surface(message, body),
            wire::METHOD_ATTACHBUFFER => self.attach_buffer(message, body),
            wire::METHOD_ATTACHBUFFERSLOT => self.attach_buffer_slot(message, body),
            wire::METHOD_COMMIT => self.commit(message, body),
            wire::METHOD_DESTROYSURFACE => self.destroy_surface(message, body),
            wire::METHOD_DRAGSTART => self.drag_start(message, body),
            wire::METHOD_DRAGCANCEL => self.drag_cancel_request(message, body),
            wire::METHOD_SUBSCRIBE => self.subscribe(message, body),
            wire::METHOD_LISTSURFACES => self.list_surfaces(message),
            wire::METHOD_GETWORKAREA => self.get_work_area(message),
            wire::METHOD_GETTHEME => super::request_shell::get_theme(message),
            _ => error_reply(message.method(), messenger::errno::EINVAL),
        }
    }

    /// `CreateSurface`: a window, or the privileged desktop layer.
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
        if width == 0 || height == 0 || width > max_w || height > max_h || message.handles == 0 {
            drop_rejected_handle(message);
            return error_reply(message.method(), messenger::errno::EINVAL);
        }
        if role == wire::ROLE_DESKTOP && !is_privileged(message.sender) {
            // Only an authorized shell identity may own the desktop (issue
            // #175); anyone else's claim is refused outright.
            drop_rejected_handle(message);
            return error_reply(message.method(), messenger::errno::EACCES);
        }
        let id = self.next_id;
        self.next_id += 1;
        let (w, h) = (width as i32, height as i32);
        if role == wire::ROLE_DESKTOP {
            // The bottom layer: no chrome, no taskbar entry, never focused. A
            // new desktop replaces the current one.
            self.replace_desktop();
            self.surfaces
                .push(new_surface(message, id, title, (0, 0), (w, h), true));
            self.notify_surface(id, wire::CHANGE_CREATED);
            self.repaint_full();
        } else {
            let origin = place_window(
                (self.screen.width(), self.screen.height()),
                &self.surfaces,
                w,
                h,
            );
            self.surfaces
                .push(new_surface(message, id, title, origin, (w, h), false));
            let before = self.focused;
            if self.focused.is_none() {
                self.focused = Some(id);
            }
            if self.focused != before {
                self.notify_focus();
            }
            self.notify_surface(id, wire::CHANGE_CREATED);
            // Open with a zoom out of the window's taskbar entry, hidden (as
            // if minimized) so the wireframe flies over the old screen.
            self.set_minimized(id, true);
            self.deiconify(id);
            self.set_minimized(id, false);
            // A new surface changes the layout (and the taskbar), so repaint
            // the whole screen.
            self.repaint_full();
        }
        typed_reply(
            message.method(),
            wire::encode_create_surface_reply(&wire::CreateSurfaceReply { surface: id }),
        )
    }

    /// Drop the current desktop surface, if any: tell its owner and close the
    /// endpoint the compositor held for it (issue #175: both were leaked).
    fn replace_desktop(&mut self) {
        let Some(index) = self.surfaces.iter().position(|surface| surface.desktop) else {
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
        // A window's damage is relative to its content origin; the desktop has
        // no chrome, so its origin is the surface origin.
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
        self.repaint(damage);
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
    desktop: bool,
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
        minimized: false,
        desktop,
        slots: Default::default(),
    }
}
