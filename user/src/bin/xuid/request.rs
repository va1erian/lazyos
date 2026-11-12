//! Display protocol request handling (issue #194 split): the surface
//! lifecycle methods of `os.lazy.display.v1`, called by `serve.rs`; drag & drop and the shell queries live in
//! `request_shell.rs`.

use alloc::vec::Vec;
use libmessenger::server::{Caller, Fault};
use user::messenger::display::{self, wire, Rect};
use user::messenger::errno;
use user::messenger::Endpoint;

use super::compositor::Compositor;
use super::geometry::{self, SizeHints};
use super::layout::place_window;
use super::protocol::refuse;
use super::surface::Surface;
use super::theme::{border, title_h};
use super::window::{focus_on_create, surface_by_id};

/// The most panels that may exist at once: a taskbar and a few popups need
/// far fewer, and each one costs every repaint an occlusion test.
const MAX_PANELS: usize = 16;

impl Compositor {
    /// `CreateSurface`: a window, or (shell-only) the desktop or a panel.
    pub(super) fn create_surface(
        &mut self,
        cx: &Caller,
        args: wire::CreateSurfaceArgs,
    ) -> Result<u64, Fault> {
        // Decoded, so the event endpoint is ours to close on a refusal.
        let events = args.events;
        let refuse = |code| {
            let _ = Endpoint::from_raw(events).close();
            refuse(code)
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
        if width == 0 || height == 0 || width > max_w || height > max_h {
            return Err(refuse(errno::EINVAL));
        }
        let panels = self.surfaces.iter().filter(|s| s.is_panel()).count();
        let refusal = match role {
            wire::ROLE_WINDOW => None,
            // Only the shell may own the desktop or a panel (issues #175,
            // #157); anyone else's claim is refused outright.
            wire::ROLE_DESKTOP | wire::ROLE_PANEL if !self.is_shell_caller(cx) => {
                Some(errno::EACCES)
            }
            wire::ROLE_PANEL if panels >= MAX_PANELS => Some(errno::EBUSY),
            wire::ROLE_DESKTOP | wire::ROLE_PANEL => None,
            _ => Some(errno::EINVAL),
        };
        if let Some(code) = refusal {
            return Err(refuse(code));
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
                .push(new_surface(cx, id, title, (0, 0), (w, h), role, events));
            self.notify_surface(id, wire::CHANGE_CREATED);
            self.repaint(Rect::new(0, 0, w, h));
        } else {
            // A window still flying in is hidden (minimized) and placement
            // skips hidden windows: land it so the new one does not take its
            // cell.
            self.finish_opening();
            let origin = place_window(self.work_area(), &self.surfaces, w, h);
            self.surfaces
                .push(new_surface(cx, id, title, origin, (w, h), role, events));
            // A new window comes up on top and focused, so double-clicking a
            // folder in Files shows the new window in front instead of behind
            // the one that opened it. The first window still gets focus.
            if focus_on_create(&mut self.surfaces, &mut self.focused, id) {
                self.notify_focus();
            }
            self.notify_surface(id, wire::CHANGE_CREATED);
            self.wm_mark("OPENED", id);
            // Open with a zoom out of the tile the app (or the shell) hinted
            // at, else out of the window's icon. The zoom runs from the main
            // loop (`opening.rs`), so this answers at once and the app builds
            // and paints its first frame while the wireframe flies in.
            let origin = self.take_open_origin(cx.sender);
            self.start_opening(id, origin);
            self.repaint_full();
        }
        // Register the surface with `inputd` before the client learns its id,
        // so the `Open` it sends next can find it. Ignore the connect backoff:
        // this is the one moment the registration matters.
        self.sync_input_now();
        Ok(id)
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

    /// `SetSizeHints`: declare `surface` resizable within content-size bounds.
    /// Only the owner may call it; a bad bound is `EINVAL`, an unknown surface
    /// `ENOENT`. A fixed-size window stays fixed, so old clients are
    /// unaffected.
    pub(super) fn set_size_hints(
        &mut self,
        cx: &Caller,
        args: wire::SetSizeHintsArgs,
    ) -> Result<(), Fault> {
        let Some(surface) = surface_by_id(&self.surfaces, args.surface) else {
            return Err(refuse(errno::ENOENT));
        };
        if surface.owner != cx.sender {
            return Err(refuse(errno::EACCES));
        }
        // The desktop and panels have no chrome to resize.
        if !surface.is_window() {
            return Err(refuse(errno::EINVAL));
        }
        let Some(hints) = SizeHints::new(
            args.min_w,
            args.min_h,
            args.max_w,
            args.max_h,
            (self.screen.width(), self.screen.height()),
        ) else {
            return Err(refuse(errno::EINVAL));
        };
        if let Some(surface) = self.surfaces.iter_mut().find(|s| s.id == args.surface) {
            surface.hints = Some(hints);
        }
        Ok(())
    }

    /// `RequestSize`: the owner asks for a new content size (a compact or
    /// expanded view). Needs declared size hints and a normal (neither
    /// maximized nor minimized) window; the size is clamped by
    /// [`geometry::requested_rect`] and the client is always answered with a
    /// `Configure`, even when nothing changed, so it never waits in vain.
    pub(super) fn request_size(
        &mut self,
        cx: &Caller,
        args: wire::RequestSizeArgs,
    ) -> Result<(), Fault> {
        let Some(surface) = surface_by_id(&self.surfaces, args.surface) else {
            return Err(refuse(errno::ENOENT));
        };
        if surface.owner != cx.sender {
            return Err(refuse(errno::EACCES));
        }
        // The owner may ask for its size while its window still flies in; the
        // window is only hidden for the zoom, not minimized, so land it (only
        // after the owner check: nobody else may show someone's window early).
        self.finish_opening_of(args.surface);
        let Some(surface) = surface_by_id(&self.surfaces, args.surface) else {
            return Err(refuse(errno::ENOENT));
        };
        let Some(hints) = surface.hints.filter(|_| surface.resizable()) else {
            return Err(refuse(errno::EINVAL));
        };
        if surface.minimized || surface.maximized.is_some() {
            return Err(refuse(errno::EINVAL));
        }
        let (window, events) = (surface.window(), surface.events);
        let rect =
            geometry::requested_rect(window, (args.width, args.height), &hints, self.work_area());
        if rect == window {
            self.send_configure(
                args.surface,
                events,
                window.w - border() * 2,
                window.h - title_h() - border(),
                wire::WINDOW_STATE_NORMAL,
            );
            return Ok(());
        }
        if let Some(surface) = self.surfaces.iter_mut().find(|s| s.id == args.surface) {
            surface.x = rect.x;
            surface.y = rect.y;
            surface.w = rect.w - border() * 2;
            surface.h = rect.h - title_h() - border();
        }
        self.send_configure(
            args.surface,
            events,
            rect.w - border() * 2,
            rect.h - title_h() - border(),
            wire::WINDOW_STATE_NORMAL,
        );
        self.notify_surface(args.surface, wire::CHANGE_RESIZED);
        // The old footprint may be larger than the new one: repaint it all.
        self.repaint_full();
        Ok(())
    }
}

/// A freshly created, unattached surface for `cx`'s sender, with the
/// event endpoint `events` the request carried.
fn new_surface(
    cx: &Caller,
    id: u64,
    title: alloc::string::String,
    origin: (i32, i32),
    size: (i32, i32),
    role: u32,
    events: u64,
) -> Surface {
    Surface {
        id,
        title,
        x: origin.0,
        y: origin.1,
        w: size.0,
        h: size.1,
        events,
        owner: cx.sender,
        label: cx.label_id,
        session: cx.session,
        pixels: 0,
        bytes: 0,
        buf_w: 0,
        buf_h: 0,
        hints: None,
        ask_close: false,
        close_asked: None,
        maximized: None,
        snap: None,
        minimized: false,
        role,
        icon: None,
        input_session: false,
        cursor: Default::default(),
        slots: Default::default(),
    }
}
