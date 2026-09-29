//! The `os.lazy.display.v1` client: an app's side of the userspace compositor.
//!
//! This mirrors `user::messenger::display` (the native client library) for the
//! static-musl xui app, built on the raw syscall-5 shim in [`crate::sys`] and
//! the shared `libmessenger` parcel codec. In client mode
//! ([`crate::backend::LazyOSBackend::new_client`]) an app never binds the
//! display grant; it resolves `xuid`, creates a surface with its event
//! endpoint, attaches a shared pixel buffer created through the display
//! syscall, and commits damage rectangles. Input arrives as one-way messages
//! on the event endpoint.
//!
//! Coordinate note: `xuid` reports `PointerDown`/`PointerUp` relative to the
//! surface content, but `PointerMove` currently carries screen-absolute
//! coordinates. The backend treats moves as a hint and hit-tests presses.

use libmessenger::{Decoder, Encoder, Header, Kind, Parcel, VERSION};

use crate::sys::{self, errno, msg_op, MsgArgs, MsgResult};

/// Well-known compositor name.
pub const NAME: &str = "os.lazy.display.v1";
/// Interface id (the `os.lazy.` prefix, like the registry's).
pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.lazy.");

/// Display protocol methods; mirrors `user/src/messenger/` and `xuid`.
pub mod method {
    /// Create a surface; the reply carries its id.
    pub const CREATE_SURFACE: u32 = 1;
    /// Attach (or replace) a surface's pixel buffer.
    pub const ATTACH_BUFFER: u32 = 2;
    /// Signal that a damage rectangle is ready to present.
    pub const COMMIT: u32 = 3;
    /// Drop a surface.
    pub const DESTROY_SURFACE: u32 = 4;
    /// Compositor to app: pointer moved.
    pub const POINTER_MOVE: u32 = 5;
    /// Compositor to app: pointer button pressed.
    pub const POINTER_DOWN: u32 = 6;
    /// Compositor to app: pointer button released.
    pub const POINTER_UP: u32 = 7;
    /// Compositor to app: key pressed.
    pub const KEY_DOWN: u32 = 8;
    /// Compositor to app: key released.
    pub const KEY_UP: u32 = 9;
    /// Compositor to app: the window manager closed this surface.
    pub const WINDOW_CLOSE: u32 = 10;
}

/// TLV field ids of the display protocol.
pub mod field {
    /// Surface id.
    pub const SURFACE: u16 = 1;
    /// Surface width in pixels.
    pub const WIDTH: u16 = 2;
    /// Surface height in pixels.
    pub const HEIGHT: u16 = 3;
    /// Window title string.
    pub const TITLE: u16 = 4;
    /// Damage rectangle x.
    pub const X: u16 = 5;
    /// Damage rectangle y.
    pub const Y: u16 = 6;
    /// Damage rectangle width.
    pub const W: u16 = 7;
    /// Damage rectangle height.
    pub const H: u16 = 8;
    /// Event payload, first word (key code, button, or pointer x).
    pub const A: u16 = 9;
    /// Event payload, second word (pointer y).
    pub const B: u16 = 10;
    /// Structured error code in a failure reply.
    pub const ERROR: u16 = 11;
}

/// PIT ticks [`Client::connect`] waits for the compositor's name to appear.
/// `xuid` is spawned before the app, but a slow FAT load of the 2.6 MiB binary
/// makes the race one-sided; the retry keeps a manual boot robust.
const CONNECT_TICKS: u64 = 600;

/// One input event delivered to an app by the compositor.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EventKind {
    /// The pointer moved (screen-absolute coordinates in `xuid` today).
    PointerMove,
    /// A pointer button went down (surface-relative).
    PointerDown,
    /// A pointer button went up (surface-relative).
    PointerUp,
    /// A key went down; `a` is the kernel key code.
    KeyDown,
    /// A key was released; `a` is the kernel key code.
    KeyUp,
}

/// One decoded input event. `a`/`b` carry pointer `(x, y)` or a key/button code
/// depending on the kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Event {
    /// What happened.
    pub kind: EventKind,
    /// First payload word.
    pub a: i64,
    /// Second payload word.
    pub b: i64,
}

/// Decode an input event from a received parcel, or `None` when the message is
/// not one.
pub fn decode_event(parcel: &Parcel) -> Option<Event> {
    let kind = match parcel.header.method {
        method::POINTER_MOVE => EventKind::PointerMove,
        method::POINTER_DOWN => EventKind::PointerDown,
        method::POINTER_UP => EventKind::PointerUp,
        method::KEY_DOWN => EventKind::KeyDown,
        method::KEY_UP => EventKind::KeyUp,
        _ => return None,
    };
    let mut a = 0i64;
    let mut b = 0i64;
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind != Kind::U64 {
            continue;
        }
        match field.id {
            field::A => a = field.as_u64().ok()? as i64,
            field::B => b = field.as_u64().ok()? as i64,
            _ => {}
        }
    }
    Some(Event { kind, a, b })
}

/// An app's connection to `xuid`.
#[derive(Clone, Copy, Debug)]
pub struct Client {
    endpoint: u64,
}

impl Client {
    /// Resolve [`NAME`] into this task, retrying briefly while the compositor
    /// starts, and wrap the endpoint handle.
    pub fn connect() -> Result<Client, i64> {
        let deadline = sys::clock_ticks().saturating_add(CONNECT_TICKS);
        loop {
            match sys::msg_resolve(NAME) {
                Ok(endpoint) => return Ok(Client { endpoint }),
                Err(code) => {
                    if sys::clock_ticks() >= deadline {
                        return Err(code);
                    }
                    sys::sleep_millis(10);
                }
            }
        }
    }

    /// The endpoint handle in this task's table.
    pub const fn handle(self) -> u64 {
        self.endpoint
    }

    /// `CreateSurface(width, height, title, events)`; the event endpoint
    /// handle is moved to the compositor, which sends input back on it. The
    /// reply carries the new surface id.
    pub fn create_surface(
        &self,
        width: u64,
        height: u64,
        title: &str,
        events: u64,
    ) -> Result<u64, i64> {
        let mut body = Encoder::new();
        body.u64(field::WIDTH, width).map_err(|_| -errno::EINVAL)?;
        body.u64(field::HEIGHT, height)
            .map_err(|_| -errno::EINVAL)?;
        body.string(field::TITLE, title)
            .map_err(|_| -errno::EINVAL)?;
        let parcel = Parcel {
            header: header(method::CREATE_SURFACE),
            body: body.finish(),
            handles: vec![events],
            buffers: Vec::new(),
        };
        let mut buf = [0u8; 256];
        let reply = self.call(&parcel, &mut buf)?;
        let mut decoder = Decoder::new(&reply.body);
        while let Ok(Some(field)) = decoder.next() {
            if field.kind == Kind::U64 && field.id == field::SURFACE {
                return field.as_u64().map_err(|_| -errno::EINVAL);
            }
        }
        Err(error_field(&reply).unwrap_or(-errno::EINVAL))
    }

    /// Attach `buffer` (a handle from the display syscall's `create_buffer`)
    /// as `surface`'s pixels; the sender keeps its handle and mapping.
    pub fn attach_buffer(&self, surface: u64, buffer: u64, len: u64) -> Result<(), i64> {
        let mut body = Encoder::new();
        body.u64(field::SURFACE, surface)
            .map_err(|_| -errno::EINVAL)?;
        let parcel = Parcel {
            header: header(method::ATTACH_BUFFER),
            body: body.finish(),
            handles: Vec::new(),
            buffers: vec![libmessenger::BufferDesc {
                handle: buffer,
                offset: 0,
                len,
                flags: 0,
            }],
        };
        let mut buf = [0u8; 64];
        let reply = self.call(&parcel, &mut buf)?;
        match error_field(&reply) {
            Some(code) => Err(code),
            None => Ok(()),
        }
    }

    /// Tell the compositor the `damage` rectangle of `surface` is ready.
    pub fn commit(&self, surface: u64, damage: (i32, i32, i32, i32)) -> Result<(), i64> {
        let mut body = Encoder::new();
        body.u64(field::SURFACE, surface)
            .map_err(|_| -errno::EINVAL)?;
        body.u64(field::X, damage.0.max(0) as u64)
            .map_err(|_| -errno::EINVAL)?;
        body.u64(field::Y, damage.1.max(0) as u64)
            .map_err(|_| -errno::EINVAL)?;
        body.u64(field::W, damage.2.max(0) as u64)
            .map_err(|_| -errno::EINVAL)?;
        body.u64(field::H, damage.3.max(0) as u64)
            .map_err(|_| -errno::EINVAL)?;
        let parcel = Parcel {
            header: header(method::COMMIT),
            body: body.finish(),
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        let mut buf = [0u8; 64];
        let reply = self.call(&parcel, &mut buf)?;
        match error_field(&reply) {
            Some(code) => Err(code),
            None => Ok(()),
        }
    }

    /// Drop `surface`; the compositor forgets it and repaints.
    pub fn destroy_surface(&self, surface: u64) -> Result<(), i64> {
        let mut body = Encoder::new();
        body.u64(field::SURFACE, surface)
            .map_err(|_| -errno::EINVAL)?;
        let parcel = Parcel {
            header: header(method::DESTROY_SURFACE),
            body: body.finish(),
            handles: Vec::new(),
            buffers: Vec::new(),
        };
        let mut buf = [0u8; 64];
        let reply = self.call(&parcel, &mut buf)?;
        match error_field(&reply) {
            Some(code) => Err(code),
            None => Ok(()),
        }
    }

    /// One synchronous call on the compositor endpoint.
    fn call(&self, parcel: &Parcel, buf: &mut [u8]) -> Result<Parcel, i64> {
        sys::msg_call(self.endpoint, parcel, buf, 0)
    }
}

/// A request header; `ALLOW_NESTED` keeps the app's event receive from tripping
/// the kernel's per-channel cycle check while a call is in flight.
fn header(method: u32) -> Header {
    Header {
        version: VERSION,
        flags: libmessenger::flags::ALLOW_NESTED,
        interface_id: INTERFACE,
        method,
        txn_id: 0,
        reply_to: 0,
        deadline_ns: 0,
    }
}

/// The structured error code in a reply, when the compositor refused a call.
fn error_field(parcel: &Parcel) -> Option<i64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::Error && field.id == field::ERROR {
            let (code, _message) = field.error_parts().ok()?;
            return Some(-(code as i64));
        }
    }
    None
}

/// Decode a whole received message: its method and the parcel. Returns `None`
/// when the bytes are not a parcel.
pub fn decode_message(bytes: &[u8]) -> Option<Parcel> {
    Parcel::decode(bytes).ok()
}

/// Close a handle opened by [`Client::connect`].
pub fn close(handle: u64) -> Result<(), i64> {
    let args = MsgArgs {
        handle,
        ..MsgArgs::default()
    };
    let mut result = MsgResult::default();
    let code = sys::messenger(
        msg_op::CLOSE_ENDPOINT,
        &args as *const MsgArgs as u64,
        &mut result as *mut MsgResult as u64,
    );
    if code < 0 {
        Err(code)
    } else {
        Ok(())
    }
}
