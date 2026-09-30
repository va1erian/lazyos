//! The `os.lazy.display.v1` client: an app's side of the userspace compositor.
//!
//! This is the static-musl xui app's counterpart of `user::messenger::display`
//! (the native client library), built on the raw syscall-5 shim in
//! [`crate::sys`] and the generated `os.lazy.display.v1` stubs
//! (`idl/display.midl`, consumed through `messenger-generated`) instead of a
//! copy of the wire. In client mode ([`crate::backend::LazyOSBackend::new_client`])
//! an app never binds the display grant; it resolves `xuid`, creates a surface
//! with its event endpoint, attaches a shared pixel buffer created through the
//! display syscall, and commits damage rectangles. Input arrives as one-way
//! messages on the event endpoint; pointer coordinates are surface-relative and
//! presses carry the button id.

use libmessenger::{BufferDesc, Decoder, Header, Kind, Parcel, VERSION};
use messenger_generated::os_lazy_display_v1 as wire;

use crate::sys::{self, errno, msg_op, MsgArgs, MsgResult};

/// Well-known compositor name.
pub const NAME: &str = "os.lazy.display.v1";
/// Interface id every display parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;
/// Method id of the one-way `WindowClose` event.
pub const METHOD_WINDOW_CLOSE: u32 = wire::METHOD_WINDOWCLOSE;

/// Structured error field id in a failure reply (outside the generated range).
const ERROR_FIELD: u16 = 15;

/// PIT ticks [`Client::connect`] waits for the compositor's name to appear.
/// `xuid` is spawned before the app, but a slow FAT load of the 2.6 MiB binary
/// makes the race one-sided; the retry keeps a manual boot robust.
const CONNECT_TICKS: u64 = 600;

/// One input event delivered to an app by the compositor, in surface
/// coordinates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// The pointer moved (relative to the surface; outside it mid-drag).
    PointerMove { x: i32, y: i32 },
    /// A pointer button went down; `button` is a kernel button id.
    PointerDown { x: i32, y: i32, button: u32 },
    /// A pointer button went up.
    PointerUp { x: i32, y: i32, button: u32 },
    /// A key went down; `key` is the kernel key code.
    KeyDown { key: u32 },
    /// A key was released.
    KeyUp { key: u32 },
}

/// Decode an input event from a received parcel, or `None` when the message is
/// not a well-formed one.
pub fn decode_event(parcel: &Parcel) -> Option<Event> {
    let body = &parcel.body;
    Some(match parcel.header.method {
        wire::METHOD_POINTERMOVE => {
            let args = wire::decode_pointer_move_args(body).ok()?;
            Event::PointerMove {
                x: args.x,
                y: args.y,
            }
        }
        wire::METHOD_POINTERDOWN => {
            let args = wire::decode_pointer_down_args(body).ok()?;
            Event::PointerDown {
                x: args.x,
                y: args.y,
                button: args.button,
            }
        }
        wire::METHOD_POINTERUP => {
            let args = wire::decode_pointer_up_args(body).ok()?;
            Event::PointerUp {
                x: args.x,
                y: args.y,
                button: args.button,
            }
        }
        wire::METHOD_KEYDOWN => Event::KeyDown {
            key: wire::decode_key_down_args(body).ok()?.key,
        },
        wire::METHOD_KEYUP => Event::KeyUp {
            key: wire::decode_key_up_args(body).ok()?.key,
        },
        _ => return None,
    })
}

/// An app's connection to `xuid`.
#[derive(Clone, Copy, Debug)]
pub struct Client {
    endpoint: u64,
}

impl Client {
    /// A connection that reaches nothing, for host tests of the backend's
    /// bookkeeping.
    #[cfg(test)]
    pub(crate) fn detached() -> Client {
        Client { endpoint: 0 }
    }

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
        let body = wire::encode_create_surface_args(&wire::CreateSurfaceArgs {
            width: u32::try_from(width).unwrap_or(u32::MAX),
            height: u32::try_from(height).unwrap_or(u32::MAX),
            title: title.into(),
            role: wire::ROLE_WINDOW,
        })
        .map_err(|_| -errno::EINVAL)?;
        let parcel = request(wire::METHOD_CREATESURFACE, body, vec![events], Vec::new());
        let reply = self.call(&parcel)?;
        match wire::decode_create_surface_reply(&reply.body) {
            // Surface ids start at 1; zero is a missing field.
            Ok(reply) if reply.surface != 0 => Ok(reply.surface),
            _ => Err(-errno::EINVAL),
        }
    }

    /// Attach `buffer` (a handle from the display syscall's `create_buffer`)
    /// as `surface`'s pixels; the sender keeps its handle and mapping.
    pub fn attach_buffer(&self, surface: u64, buffer: u64, len: u64) -> Result<(), i64> {
        let body = wire::encode_attach_buffer_args(&wire::AttachBufferArgs { surface })
            .map_err(|_| -errno::EINVAL)?;
        let buffers = vec![BufferDesc {
            handle: buffer,
            offset: 0,
            len,
            flags: 0,
        }];
        let parcel = request(wire::METHOD_ATTACHBUFFER, body, Vec::new(), buffers);
        self.call(&parcel).map(|_| ())
    }

    /// Tell the compositor the `damage` rectangle of `surface` is ready.
    pub fn commit(&self, surface: u64, damage: (i32, i32, i32, i32)) -> Result<(), i64> {
        let body = wire::encode_commit_args(&wire::CommitArgs {
            surface,
            x: damage.0.max(0) as u32,
            y: damage.1.max(0) as u32,
            w: damage.2.max(0) as u32,
            h: damage.3.max(0) as u32,
        })
        .map_err(|_| -errno::EINVAL)?;
        let parcel = request(wire::METHOD_COMMIT, body, Vec::new(), Vec::new());
        self.call(&parcel).map(|_| ())
    }

    /// `SetTitle`: rename `surface`'s window. An older compositor answers
    /// `EINVAL` (the method is unknown to it), which callers ignore: the title
    /// only decorates.
    pub fn set_title(&self, surface: u64, title: &str) -> Result<(), i64> {
        let body = wire::encode_set_title_args(&wire::SetTitleArgs {
            surface,
            title: title.into(),
        })
        .map_err(|_| -errno::EINVAL)?;
        let parcel = request(wire::METHOD_SETTITLE, body, Vec::new(), Vec::new());
        self.call(&parcel).map(|_| ())
    }

    /// `HintOpenOrigin`: ask the compositor to open this task's next window
    /// from `rect` (`x, y, w, h`, relative to `surface`'s content origin)
    /// instead of from its taskbar entry. Purely cosmetic: an older
    /// compositor answers `EINVAL`, which callers ignore.
    pub fn hint_open_origin(&self, surface: u64, rect: (i32, i32, u32, u32)) -> Result<(), i64> {
        let parcel = hint_open_origin_parcel(surface, rect)?;
        self.call(&parcel).map(|_| ())
    }

    /// Drop `surface`; the compositor forgets it and repaints.
    pub fn destroy_surface(&self, surface: u64) -> Result<(), i64> {
        let body = wire::encode_destroy_surface_args(&wire::DestroySurfaceArgs { surface })
            .map_err(|_| -errno::EINVAL)?;
        let parcel = request(wire::METHOD_DESTROYSURFACE, body, Vec::new(), Vec::new());
        self.call(&parcel).map(|_| ())
    }

    /// One synchronous call on the compositor endpoint; a structured error
    /// reply becomes its negative errno.
    fn call(&self, parcel: &Parcel) -> Result<Parcel, i64> {
        let mut buf = [0u8; 256];
        let reply = sys::msg_call(self.endpoint, parcel, &mut buf, 0)?;
        match error_field(&reply) {
            Some(code) => Err(code),
            None => Ok(reply),
        }
    }
}

/// The `HintOpenOrigin` request parcel for `rect` relative to `surface`.
fn hint_open_origin_parcel(surface: u64, rect: (i32, i32, u32, u32)) -> Result<Parcel, i64> {
    let body = wire::encode_hint_open_origin_args(&wire::HintOpenOriginArgs {
        surface,
        x: rect.0,
        y: rect.1,
        w: rect.2,
        h: rect.3,
    })
    .map_err(|_| -errno::EINVAL)?;
    Ok(request(
        wire::METHOD_HINTOPENORIGIN,
        body,
        Vec::new(),
        Vec::new(),
    ))
}

/// A request parcel; `ALLOW_NESTED` keeps the app's event receive from
/// tripping the kernel's per-channel cycle check while a call is in flight.
fn request(method: u32, body: Vec<u8>, handles: Vec<u64>, buffers: Vec<BufferDesc>) -> Parcel {
    Parcel {
        header: Header {
            version: VERSION,
            flags: libmessenger::flags::ALLOW_NESTED,
            interface_id: INTERFACE,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        handles,
        buffers,
    }
}

/// The structured error code in a reply, when the compositor refused a call.
fn error_field(parcel: &Parcel) -> Option<i64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::Error && field.id == ERROR_FIELD {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hint_parcel_carries_method_30_and_the_rect() {
        let parcel = hint_open_origin_parcel(7, (-4, 12, 64, 48)).expect("encodes");
        assert_eq!(parcel.header.method, 30);
        assert_eq!(parcel.header.interface_id, INTERFACE);
        let args = wire::decode_hint_open_origin_args(&parcel.body).expect("decodes");
        assert_eq!(
            (args.surface, args.x, args.y, args.w, args.h),
            (7, -4, 12, 64, 48)
        );
    }
}
