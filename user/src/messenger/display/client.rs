//! [`Client`]: an app's connection to the compositor.

use alloc::vec;
use alloc::vec::Vec;

use libmessenger::{BufferDesc, Decoder, Encoder, Kind, Parcel};

use super::super::{errno, registry, Endpoint, Error, Result};
use super::canvas::Rect;
use super::events::{color_from_u64, decode_surface_list, SurfaceInfo, Theme};
use super::{field, header, method, role, CONNECT_TICKS};

/// An app's connection to the compositor.
#[derive(Clone, Copy)]
pub struct Client {
    endpoint: Endpoint,
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
        self.create_surface_role(width, height, title, events, role::WINDOW)
    }

    /// A `CreateSurface` with [`role::DESKTOP`] (issue #167): the surface
    /// paints at the bottom of the z-order, above the background colour and
    /// below every window. It has no chrome, never takes focus and never
    /// appears in the taskbar or the Alt+Tab cycle; creating a new desktop
    /// replaces the previous one. The event endpoint is still transferred,
    /// so a future desktop can receive input.
    pub fn create_desktop_surface(
        &self,
        width: u64,
        height: u64,
        title: &str,
        events: &Endpoint,
    ) -> Result<u64> {
        self.create_surface_role(width, height, title, events, role::DESKTOP)
    }

    /// The shared body of [`Client::create_surface`] and
    /// [`Client::create_desktop_surface`].
    fn create_surface_role(
        &self,
        width: u64,
        height: u64,
        title: &str,
        events: &Endpoint,
        role: u64,
    ) -> Result<u64> {
        let mut body = Encoder::new();
        body.u64(field::WIDTH, width).map_err(Error::Parcel)?;
        body.u64(field::HEIGHT, height).map_err(Error::Parcel)?;
        body.string(field::TITLE, title).map_err(Error::Parcel)?;
        body.u64(field::ROLE, role).map_err(Error::Parcel)?;
        let parcel = Parcel {
            header: header(method::CREATE_SURFACE),
            body: body.finish(),
            handles: vec![events.handle()],
            buffers: Vec::new(),
        };
        let mut buf = [0u8; 256];
        let reply = self.endpoint.call_with(&parcel, &mut buf, None)?;
        // A refusal (e.g. `-EACCES` for the desktop role) is a structured
        // error reply, not a missing surface id.
        if let Some(code) = error_field(&reply) {
            return Err(Error::Errno(-code));
        }
        let mut decoder = Decoder::new(&reply.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind == Kind::U64 && field.id == field::SURFACE {
                return field.as_u64().map_err(Error::Parcel);
            }
        }
        Err(Error::Errno(-errno::EINVAL))
    }

    /// `Subscribe(role, events)`: register this task as the shell
    /// subscriber (issue #167). The event endpoint is moved to the
    /// compositor, which sends one-way [`super::events::ShellEvent`]s there. The role
    /// [`super::ROLE_SHELL`] also hides xuid's built-in taskbar; any other role
    /// keeps the fallback chrome. Registering again replaces the endpoint.
    pub fn subscribe(&self, role: &str, events: &Endpoint) -> Result<()> {
        let mut body = Encoder::new();
        body.string(field::SUBSCRIBER_ROLE, role)
            .map_err(Error::Parcel)?;
        let parcel = Parcel {
            header: header(method::SUBSCRIBE),
            body: body.finish(),
            handles: vec![events.handle()],
            buffers: Vec::new(),
        };
        let mut buf = [0u8; 256]; // an error reply carries a message
        let reply = self.endpoint.call_with(&parcel, &mut buf, None)?;
        match error_field(&reply) {
            Some(code) => Err(Error::Errno(-code)),
            None => Ok(()),
        }
    }

    /// `ListSurfaces`: every surface the compositor knows, in its z-order
    /// (bottom first). Desktop surfaces are included and their rows show
    /// the composited geometry (issue #167).
    pub fn list_surfaces(&self) -> Result<Vec<SurfaceInfo>> {
        let parcel = Parcel {
            header: header(method::LIST_SURFACES),
            body: Encoder::new().finish(),
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        let reply = self.endpoint.call(&parcel, None)?;
        if let Some(code) = error_field(&reply) {
            return Err(Error::Errno(-code));
        }
        decode_surface_list(&reply.body)
    }

    /// `GetWorkArea`: the rectangle windows may occupy. While the built-in
    /// fallback taskbar is visible the bar's strip is excluded; with a
    /// shell registered (`Subscribe("shell", ..)`) the bar is hidden and
    /// the work area is the whole screen (issue #167).
    pub fn get_work_area(&self) -> Result<Rect> {
        let parcel = Parcel {
            header: header(method::GET_WORK_AREA),
            body: Encoder::new().finish(),
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        let mut buf = [0u8; 256];
        let reply = self.endpoint.call_with(&parcel, &mut buf, None)?;
        if let Some(code) = error_field(&reply) {
            return Err(Error::Errno(-code));
        }
        let (mut x, mut y, mut w, mut h) = (0i32, 0i32, 0i32, 0i32);
        let mut decoder = Decoder::new(&reply.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind != Kind::U64 {
                continue;
            }
            let value = field.as_u64().map_err(Error::Parcel)? as i32;
            match field.id {
                self::field::X => x = value,
                self::field::Y => y = value,
                self::field::W => w = value,
                self::field::H => h = value,
                _ => {}
            }
        }
        Ok(Rect::new(x, y, w, h))
    }

    /// `GetTheme`: xuid's current chrome palette, so the shell's own
    /// surfaces can match it (issue #167).
    pub fn get_theme(&self) -> Result<Theme> {
        let parcel = Parcel {
            header: header(method::GET_THEME),
            body: Encoder::new().finish(),
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        let mut buf = [0u8; 256];
        let reply = self.endpoint.call_with(&parcel, &mut buf, None)?;
        if let Some(code) = error_field(&reply) {
            return Err(Error::Errno(-code));
        }
        let mut theme = Theme::default();
        let mut decoder = Decoder::new(&reply.body);
        while let Some(field) = decoder.next().map_err(Error::Parcel)? {
            if field.kind != Kind::U64 {
                continue;
            }
            let color = color_from_u64(field.as_u64().map_err(Error::Parcel)?);
            match field.id {
                self::field::TITLE_BG_ACTIVE => theme.title_bg_active = color,
                self::field::TITLE_BG_INACTIVE => theme.title_bg_inactive = color,
                self::field::BORDER => theme.border = color,
                self::field::TASKBAR => theme.taskbar = color,
                self::field::TEXT => theme.text = color,
                _ => {}
            }
        }
        Ok(theme)
    }

    /// Share `buffer` (a handle from the `display` syscall's
    /// `create_buffer`) with the compositor as `surface`'s pixels. The
    /// sender keeps its handle and mapping; the compositor gains one.
    pub fn attach_buffer(&self, surface: u64, buffer: u64, len: u64) -> Result<()> {
        let mut body = Encoder::new();
        body.u64(field::SURFACE, surface).map_err(Error::Parcel)?;
        let parcel = Parcel {
            header: header(method::ATTACH_BUFFER),
            body: body.finish(),
            handles: Vec::new(),
            buffers: vec![BufferDesc {
                handle: buffer,
                offset: 0,
                len,
                flags: 0,
            }],
        };
        let mut buf = [0u8; 256]; // an error reply carries a message
        let reply = self.endpoint.call_with(&parcel, &mut buf, None)?;
        if let Some(code) = error_field(&reply) {
            return Err(Error::Errno(-code));
        }
        Ok(())
    }

    /// Tell the compositor the `damage` rectangle of `surface` is ready.
    pub fn commit(&self, surface: u64, damage: Rect) -> Result<()> {
        let mut body = Encoder::new();
        body.u64(field::SURFACE, surface).map_err(Error::Parcel)?;
        body.u64(field::X, damage.x.max(0) as u64)
            .map_err(Error::Parcel)?;
        body.u64(field::Y, damage.y.max(0) as u64)
            .map_err(Error::Parcel)?;
        body.u64(field::W, damage.w.max(0) as u64)
            .map_err(Error::Parcel)?;
        body.u64(field::H, damage.h.max(0) as u64)
            .map_err(Error::Parcel)?;
        let parcel = Parcel {
            header: header(method::COMMIT),
            body: body.finish(),
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        let mut buf = [0u8; 256]; // an error reply carries a message
        let reply = self.endpoint.call_with(&parcel, &mut buf, None)?;
        if let Some(code) = error_field(&reply) {
            return Err(Error::Errno(-code));
        }
        Ok(())
    }

    /// Drop `surface`; the compositor forgets it and repaints.
    pub fn destroy_surface(&self, surface: u64) -> Result<()> {
        let mut body = Encoder::new();
        body.u64(field::SURFACE, surface).map_err(Error::Parcel)?;
        let parcel = Parcel {
            header: header(method::DESTROY_SURFACE),
            body: body.finish(),
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        let mut buf = [0u8; 256]; // an error reply carries a message
        let reply = self.endpoint.call_with(&parcel, &mut buf, None)?;
        if let Some(code) = error_field(&reply) {
            return Err(Error::Errno(-code));
        }
        Ok(())
    }

    /// `DragStart(surface, token, mime)`: hand `surface`'s in-progress
    /// gesture to the compositor, which tracks the pointer and delivers a
    /// `Drop` carrying `token`. The payload is offered to `clipboardd`
    /// first (issue #145); the compositor never sees the bytes.
    pub fn drag_start(&self, surface: u64, token: u64, mime: &str) -> Result<()> {
        let mut body = Encoder::new();
        body.u64(field::SURFACE, surface).map_err(Error::Parcel)?;
        body.u64(field::TOKEN, token).map_err(Error::Parcel)?;
        body.string(field::MIME, mime).map_err(Error::Parcel)?;
        let parcel = Parcel {
            header: header(method::DRAG_START),
            body: body.finish(),
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        let mut buf = [0u8; 256]; // an error reply carries a message
        let reply = self.endpoint.call_with(&parcel, &mut buf, None)?;
        match error_field(&reply) {
            Some(code) => Err(Error::Errno(-code)),
            None => Ok(()),
        }
    }

    /// `DragCancel(surface)`: cancel the drag that started at `surface`.
    pub fn drag_cancel(&self, surface: u64) -> Result<()> {
        let mut body = Encoder::new();
        body.u64(field::SURFACE, surface).map_err(Error::Parcel)?;
        let parcel = Parcel {
            header: header(method::DRAG_CANCEL),
            body: body.finish(),
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        let mut buf = [0u8; 256]; // an error reply carries a message
        let reply = self.endpoint.call_with(&parcel, &mut buf, None)?;
        match error_field(&reply) {
            Some(code) => Err(Error::Errno(-code)),
            None => Ok(()),
        }
    }
}

/// The structured error code in a reply, when the compositor refused a
/// call (a positive errno, as `xuid` stores it).
fn error_field(parcel: &Parcel) -> Option<i64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::Error && field.id == field::ERROR {
            let (code, _message) = field.error_parts().ok()?;
            return Some(code as i64);
        }
    }
    None
}
