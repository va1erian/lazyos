//! [`Client`]: an app's connection to the compositor, built on the generated
//! `os.lazy.display.v1` stubs.

use alloc::vec;
use alloc::vec::Vec;

use libmessenger::{BufferDesc, Parcel};

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

    /// The shared body of [`Client::create_surface`] and
    /// [`Client::create_desktop_surface`].
    fn create_surface_role(
        &self,
        width: u64,
        height: u64,
        title: &str,
        events: &Endpoint,
        role: u32,
    ) -> Result<u64> {
        let body = wire::encode_create_surface_args(&wire::CreateSurfaceArgs {
            width: wire_u32(width),
            height: wire_u32(height),
            title: title.into(),
            role,
        })
        .map_err(Error::Parcel)?;
        let reply = self.call(request(
            wire::METHOD_CREATESURFACE,
            body,
            vec![events.handle()],
            Vec::new(),
        ))?;
        let surface = wire::decode_create_surface_reply(&reply.body)
            .map_err(Error::Parcel)?
            .surface;
        // Surface ids start at 1: a missing id is a malformed reply.
        if surface == 0 {
            return Err(Error::Errno(-errno::EINVAL));
        }
        Ok(surface)
    }

    /// `Subscribe(role, events)`: register this task as the shell
    /// subscriber (issue #167). The event endpoint is moved to the
    /// compositor, which sends one-way [`super::ShellEvent`]s there. The role
    /// [`super::ROLE_SHELL`] also hides xuid's built-in taskbar; any other
    /// role keeps the fallback chrome. Registering again replaces the
    /// endpoint.
    pub fn subscribe(&self, role: &str, events: &Endpoint) -> Result<()> {
        let body = wire::encode_subscribe_args(&wire::SubscribeArgs {
            subscriber_role: role.into(),
        })
        .map_err(Error::Parcel)?;
        self.call(request(
            wire::METHOD_SUBSCRIBE,
            body,
            vec![events.handle()],
            Vec::new(),
        ))
        .map(|_| ())
    }

    /// `ListSurfaces`: every surface the compositor knows, in its z-order
    /// (bottom first). Desktop surfaces are included and their rows show
    /// the composited geometry (issue #167).
    pub fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        let parcel = request(
            wire::METHOD_LISTSURFACES,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let reply = self.endpoint.call(&parcel, None)?;
        if let Some(code) = error_field(&reply) {
            return Err(Error::Errno(-code));
        }
        Ok(wire::decode_list_surfaces_reply(&reply.body)
            .map_err(Error::Parcel)?
            .surfaces)
    }

    /// `GetWorkArea`: the rectangle windows may occupy. While the built-in
    /// fallback taskbar is visible the bar's strip is excluded; with a
    /// shell registered (`Subscribe("shell", ..)`) the bar is hidden and
    /// the work area is the whole screen (issue #167).
    pub fn get_work_area(&self) -> Result<Rect> {
        let reply = self.call(request(
            wire::METHOD_GETWORKAREA,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ))?;
        let area = wire::decode_get_work_area_reply(&reply.body).map_err(Error::Parcel)?;
        Ok(Rect::new(area.x, area.y, area.w, area.h))
    }

    /// `GetTheme`: xuid's current chrome palette, so the shell's own
    /// surfaces can match it (issue #167).
    pub fn get_theme(&self) -> Result<Theme> {
        let reply = self.call(request(
            wire::METHOD_GETTHEME,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        ))?;
        let theme = wire::decode_get_theme_reply(&reply.body).map_err(Error::Parcel)?;
        Ok(Theme::from_reply(&theme))
    }

    /// Share `buffer` (a handle from the `display` syscall's
    /// `create_buffer`) with the compositor as `surface`'s pixels. The
    /// sender keeps its handle and mapping; the compositor gains one.
    pub fn attach_buffer(&self, surface: u64, buffer: u64, len: u64) -> Result<()> {
        let body = wire::encode_attach_buffer_args(&wire::AttachBufferArgs { surface })
            .map_err(Error::Parcel)?;
        let buffers = vec![BufferDesc {
            handle: buffer,
            offset: 0,
            len,
            flags: 0,
        }];
        self.call(request(
            wire::METHOD_ATTACHBUFFER,
            body,
            Vec::new(),
            buffers,
        ))
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
        self.call(request(wire::METHOD_COMMIT, body, Vec::new(), Vec::new()))
            .map(|_| ())
    }

    /// Drop `surface`; the compositor forgets it and repaints.
    pub fn destroy_surface(&self, surface: u64) -> Result<()> {
        let body = wire::encode_destroy_surface_args(&wire::DestroySurfaceArgs { surface })
            .map_err(Error::Parcel)?;
        self.call(request(
            wire::METHOD_DESTROYSURFACE,
            body,
            Vec::new(),
            Vec::new(),
        ))
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
        self.call(request(
            wire::METHOD_DRAGSTART,
            body,
            Vec::new(),
            Vec::new(),
        ))
        .map(|_| ())
    }

    /// `DragCancel(surface)`: cancel the drag that started at `surface`.
    pub fn drag_cancel(&self, surface: u64) -> Result<()> {
        let body = wire::encode_drag_cancel_args(&wire::DragCancelArgs { surface })
            .map_err(Error::Parcel)?;
        self.call(request(
            wire::METHOD_DRAGCANCEL,
            body,
            Vec::new(),
            Vec::new(),
        ))
        .map(|_| ())
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
