//! [`Client`]: an app's connection to the compositor, built on the generated
//! `os.lazy.display.v1` stubs.

use alloc::vec::Vec;

use libmessenger::{flags, Buffer, Parcel};

use super::super::{errno, registry, Endpoint, Error, Result};
use super::canvas::Rect;
use super::events::{SurfaceInfo, Theme};
use super::{error_field, request, wire, CONNECT_TICKS};

/// An app's connection to the compositor.
#[derive(Clone, Copy)]
pub struct Client {
    endpoint: Endpoint,
}

/// Clamp a caller's size to the wire's `u32`; the compositor bounds it to the
/// screen anyway, so an oversized claim is refused there.
fn wire_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

impl Client {
    /// Resolve [`super::NAME`] into this task, retrying briefly while the
    /// compositor starts, and wrap the endpoint.
    pub fn connect() -> Result<Client> {
        let deadline = crate::sys::clock().saturating_add(CONNECT_TICKS);
        loop {
            match registry::resolve(super::NAME) {
                Ok(endpoint) => return Ok(Client { endpoint }),
                Err(error) => {
                    if crate::sys::clock() >= deadline {
                        return Err(error);
                    }
                    // Park one tick on the child-exit queue: no children
                    // means this is a clean sleep (the "no clock yet"
                    // pattern the other clients use).
                    let _ = crate::sys::wait(crate::sys::clock() + 1);
                }
            }
        }
    }

    /// A `CreateSurface(width, height, title, events)` request. The event
    /// endpoint is moved to the compositor, which sends input back on it.
    /// Returns the new surface id.
    pub fn create_surface(
        &self,
        width: u64,
        height: u64,
        title: &str,
        events: &Endpoint,
    ) -> Result<u64> {
        self.create_surface_role(width, height, title, events, wire::ROLE_WINDOW)
    }

    /// A `CreateSurface` with [`wire::ROLE_DESKTOP`] (issue #167): the
    /// surface paints at the bottom of the z-order, above the background
    /// colour and below every window. It has no chrome, never takes focus and
    /// never appears in the taskbar or the Alt+Tab cycle; creating a new
    /// desktop replaces the previous one. The event endpoint is still
    /// transferred, so a future desktop can receive input.
    pub fn create_desktop_surface(
        &self,
        width: u64,
        height: u64,
        title: &str,
        events: &Endpoint,
    ) -> Result<u64> {
        self.create_surface_role(width, height, title, events, wire::ROLE_DESKTOP)
    }

    /// A `CreateSurface` with [`wire::ROLE_PANEL`] (issue #157): a shell-only,
    /// chromeless surface painted above every window (a taskbar, a start
    /// menu). It opens at `(0, 0)`; move it with [`Client::place_surface`]
    /// before its first commit. It never takes focus but receives the pointer
    /// events over it.
    pub fn create_panel_surface(
        &self,
        width: u64,
        height: u64,
        title: &str,
        events: &Endpoint,
    ) -> Result<u64> {
        self.create_surface_role(width, height, title, events, wire::ROLE_PANEL)
    }

    /// The shared body of the `create_*surface` calls.
    fn create_surface_role(
        &self,
        width: u64,
        height: u64,
        title: &str,
        events: &Endpoint,
        role: u32,
    ) -> Result<u64> {
        let (body, objects) = wire::encode_create_surface_args(&wire::CreateSurfaceArgs {
            width: wire_u32(width),
            height: wire_u32(height),
            title: title.into(),
            role,
            popup: None,
            events: events.handle(),
        })
        .map_err(Error::Parcel)?;
        let reply = self.call(request(wire::METHOD_CREATESURFACE, body, objects))?;
        let surface = wire::decode_create_surface_reply(&reply.body)
            .map_err(Error::Parcel)?
            .surface;
        // Surface ids start at 1: a missing id is a malformed reply.
        if surface == 0 {
            return Err(Error::Errno(-errno::EINVAL));
        }
        Ok(surface)
    }

    /// `Subscribe(role, events)`: register this task for the shell events
    /// (issues #167, #157). The event endpoint is moved to the compositor,
    /// which sends one-way [`super::ShellEvent`]s there. The role
    /// [`super::ROLE_SHELL`] makes this task *the* shell (privileged, or the
    /// session that owns the display); any other role is a privileged
    /// observer that never replaces the shell. Registering again replaces the
    /// endpoint.
    pub fn subscribe(&self, role: &str, events: &Endpoint) -> Result<()> {
        let (body, objects) = wire::encode_subscribe_args(&wire::SubscribeArgs {
            subscriber_role: role.into(),
            events: events.handle(),
        })
        .map_err(Error::Parcel)?;
        self.call(request(wire::METHOD_SUBSCRIBE, body, objects))
            .map(|_| ())
    }

    /// `ListSurfaces` (shell-only): every surface the compositor knows,
    /// bottom first (the desktop, the windows in z-order, the panels), with
    /// the composited geometry (issue #167).
    pub fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        let parcel = request(wire::METHOD_LISTSURFACES, Vec::new(), Vec::new());
        let reply = self.endpoint.call(&parcel, None)?;
        if let Some(code) = error_field(&reply) {
            return Err(Error::Errno(-code));
        }
        Ok(wire::decode_list_surfaces_reply(&reply.body)
            .map_err(Error::Parcel)?
            .surfaces)
    }

    /// `GetWorkArea`: the rectangle windows may occupy: what the shell set
    /// with [`Client::set_work_area`], else the whole screen.
    pub fn get_work_area(&self) -> Result<Rect> {
        let reply = self.call(request(wire::METHOD_GETWORKAREA, Vec::new(), Vec::new()))?;
        let area = wire::decode_get_work_area_reply(&reply.body).map_err(Error::Parcel)?;
        Ok(Rect::new(area.x, area.y, area.w, area.h))
    }

    /// `GetTheme`: xuid's current chrome palette, so the shell's own
    /// surfaces can match it (issue #167).
    pub fn get_theme(&self) -> Result<Theme> {
        let reply = self.call(request(wire::METHOD_GETTHEME, Vec::new(), Vec::new()))?;
        let theme = wire::decode_get_theme_reply(&reply.body).map_err(Error::Parcel)?;
        Ok(Theme::from_reply(&theme))
    }

    /// Share `buffer` (a handle from the `display` syscall's
    /// `create_buffer`) with the compositor as `surface`'s pixels. The
    /// sender keeps its handle and mapping; the compositor gains one.
    pub fn attach_buffer(&self, surface: u64, buffer: u64, len: u64) -> Result<()> {
        let (body, objects) = wire::encode_attach_buffer_args(&wire::AttachBufferArgs {
            surface,
            pixels: Buffer::whole(buffer, len),
        })
        .map_err(Error::Parcel)?;
        self.call(request(wire::METHOD_ATTACHBUFFER, body, objects))
            .map(|_| ())
    }

    /// Tell the compositor the `damage` rectangle of `surface` is ready.
    pub fn commit(&self, surface: u64, damage: Rect) -> Result<()> {
        let body = wire::encode_commit_args(&wire::CommitArgs {
            surface,
            x: damage.x.max(0) as u32,
            y: damage.y.max(0) as u32,
            w: damage.w.max(0) as u32,
            h: damage.h.max(0) as u32,
        })
        .map_err(Error::Parcel)?;
        self.call(request(wire::METHOD_COMMIT, body, Vec::new()))
            .map(|_| ())
    }

    /// `SetSizeHints`: declare `surface` resizable within the given content
    /// bounds (a `max` of 0 means the screen). Only the surface's creator may
    /// call it. An older compositor answers `EINVAL`; callers may ignore it,
    /// leaving the window fixed-size. A caller that opts in must handle
    /// [`super::Event::Configure`]: allocate a buffer of the new size, attach
    /// it (`attach_buffer`, or `attach_slot` on a non-current slot), and
    /// redraw; the compositor refuses an attach smaller than the new size.
    pub fn set_size_hints(
        &self,
        surface: u64,
        min_w: u32,
        min_h: u32,
        max_w: u32,
        max_h: u32,
    ) -> Result<()> {
        let body = wire::encode_set_size_hints_args(&wire::SetSizeHintsArgs {
            surface,
            min_w,
            min_h,
            max_w,
            max_h,
        })
        .map_err(Error::Parcel)?;
        self.call(request(wire::METHOD_SETSIZEHINTS, body, Vec::new()))
            .map(|_| ())
    }

    /// `AttachBufferSlot`: share `buffer` with the compositor as buffer slot
    /// `slot` (`0..surfbuf::MAX_SLOTS`) of `surface` (issue #361). Fails with
    /// `EBUSY` while `slot` is the surface's current buffer.
    pub fn attach_slot(&self, surface: u64, slot: u32, buffer: u64, len: u64) -> Result<()> {
        let (body, objects) = wire::encode_attach_buffer_slot_args(&wire::AttachBufferSlotArgs {
            surface,
            slot,
            pixels: Buffer::whole(buffer, len),
        })
        .map_err(Error::Parcel)?;
        self.call(request(wire::METHOD_ATTACHBUFFERSLOT, body, objects))
            .map(|_| ())
    }

    /// `Present` (issue #361): a one-way, pipelined commit. Makes `slot` the
    /// surface's current buffer and composites `damage` (an empty slice or
    /// more than [`surfbuf::MAX_DAMAGE`] rectangles means the whole surface).
    /// The compositor answers with [`super::FrameEvent`]s on the surface's
    /// event endpoint: `BufferRelease` for the slot this replaced, then
    /// `FrameDone(seq)`.
    pub fn present(&self, surface: u64, slot: u32, seq: u64, damage: &[Rect]) -> Result<()> {
        let damage = damage
            .iter()
            .map(|rect| wire::Rect {
                x: rect.x.max(0) as u32,
                y: rect.y.max(0) as u32,
                w: rect.w.max(0) as u32,
                h: rect.h.max(0) as u32,
            })
            .collect();
        let body = wire::encode_present_args(&wire::PresentArgs {
            surface,
            slot,
            seq,
            damage,
        })
        .map_err(Error::Parcel)?;
        let mut parcel = request(wire::METHOD_PRESENT, body, Vec::new());
        parcel.header.flags |= flags::ONE_WAY;
        self.endpoint.send(&parcel)
    }

    /// Drop `surface`; the compositor forgets it and repaints.
    pub fn destroy_surface(&self, surface: u64) -> Result<()> {
        let body = wire::encode_destroy_surface_args(&wire::DestroySurfaceArgs { surface })
            .map_err(Error::Parcel)?;
        self.call(request(wire::METHOD_DESTROYSURFACE, body, Vec::new()))
            .map(|_| ())
    }

    /// `DragStart(surface, token, mime)`: hand `surface`'s in-progress
    /// gesture to the compositor, which tracks the pointer and delivers a
    /// `Drop` carrying `token`. The payload is offered to `clipboardd`
    /// first (issue #145); the compositor never sees the bytes.
    pub fn drag_start(&self, surface: u64, token: u64, mime: &str) -> Result<()> {
        let body = wire::encode_drag_start_args(&wire::DragStartArgs {
            surface,
            token,
            mime: mime.into(),
        })
        .map_err(Error::Parcel)?;
        self.call(request(wire::METHOD_DRAGSTART, body, Vec::new()))
            .map(|_| ())
    }

    /// `DragCancel(surface)`: cancel the drag that started at `surface`.
    pub fn drag_cancel(&self, surface: u64) -> Result<()> {
        let body = wire::encode_drag_cancel_args(&wire::DragCancelArgs { surface })
            .map_err(Error::Parcel)?;
        self.call(request(wire::METHOD_DRAGCANCEL, body, Vec::new()))
            .map(|_| ())
    }

    /// `PlaceSurface`: move this task's panel so its top-left is at screen
    /// `(x, y)` (clamped on screen).
    pub fn place_surface(&self, surface: u64, x: i32, y: i32) -> Result<()> {
        let body = wire::encode_place_surface_args(&wire::PlaceSurfaceArgs { surface, x, y });
        self.call_unit(wire::METHOD_PLACESURFACE, body)
    }

    /// `ActivateSurface` (shell-only): restore, raise and focus a window.
    pub fn activate_surface(&self, surface: u64) -> Result<()> {
        let body = wire::encode_activate_surface_args(&wire::ActivateSurfaceArgs { surface });
        self.call_unit(wire::METHOD_ACTIVATESURFACE, body)
    }

    /// `MinimizeSurface` (shell-only): minimize a window, like its title-bar
    /// button.
    pub fn minimize_surface(&self, surface: u64) -> Result<()> {
        let body = wire::encode_minimize_surface_args(&wire::MinimizeSurfaceArgs { surface });
        self.call_unit(wire::METHOD_MINIMIZESURFACE, body)
    }

    /// `SetWorkArea` (shell-only): where windows may go, e.g. the screen
    /// minus the shell's taskbar.
    pub fn set_work_area(&self, area: Rect) -> Result<()> {
        let (x, y, w, h) = (area.x, area.y, area.w, area.h);
        let body = wire::encode_set_work_area_args(&wire::SetWorkAreaArgs { x, y, w, h });
        self.call_unit(wire::METHOD_SETWORKAREA, body)
    }

    /// `SetIconGeometry` (shell-only): where `surface`'s taskbar entry is,
    /// so its minimize/restore zoom flies there; an empty rect forgets it.
    pub fn set_icon_geometry(&self, surface: u64, rect: Rect) -> Result<()> {
        let (x, y, w, h) = (rect.x, rect.y, rect.w, rect.h);
        let args = wire::SetIconGeometryArgs {
            surface,
            x,
            y,
            w,
            h,
        };
        let body = wire::encode_set_icon_geometry_args(&args);
        self.call_unit(wire::METHOD_SETICONGEOMETRY, body)
    }

    /// `HintLaunchOrigin` (shell-only): the next window any task opens soon
    /// zooms out of `rect` (the menu row or icon that launched it).
    pub fn hint_launch_origin(&self, rect: Rect) -> Result<()> {
        let args = wire::HintLaunchOriginArgs {
            x: rect.x,
            y: rect.y,
            w: rect.w.max(0) as u32,
            h: rect.h.max(0) as u32,
        };
        let body = wire::encode_hint_launch_origin_args(&args);
        self.call_unit(wire::METHOD_HINTLAUNCHORIGIN, body)
    }

    /// A call with an encoded `body` and nothing in the reply.
    fn call_unit(
        &self,
        method: u32,
        body: core::result::Result<Vec<u8>, libmessenger::Error>,
    ) -> Result<()> {
        let body = body.map_err(Error::Parcel)?;
        self.call(request(method, body, Vec::new())).map(|_| ())
    }

    /// One synchronous call; a structured error reply becomes
    /// [`Error::Errno`]. The small reply buffer is enough for every call but
    /// `ListSurfaces`, which reads its own.
    fn call(&self, parcel: Parcel) -> Result<Parcel> {
        let mut buf = [0u8; 256];
        let reply = self.endpoint.call_with(&parcel, &mut buf, None)?;
        match error_field(&reply) {
            Some(code) => Err(Error::Errno(-code)),
            None => Ok(reply),
        }
    }
}
