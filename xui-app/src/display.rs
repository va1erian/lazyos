//! The `os.lazy.display.v1` client: an app's side of the userspace compositor.
//!
//! This is the static-musl xui app's counterpart of `user::messenger::display`
//! (the native client library), built on the generated `os.lazy.display.v1`
//! client (`idl/display.midl`, consumed through `messenger-generated`) instead
//! of a copy of the wire; every call waits the `deadline` the `.midl` states. In client mode ([`crate::backend::LazyOSBackend::new_client`])
//! an app never binds the display grant; it resolves `xuid`, creates a surface
//! with its event endpoint, attaches shared pixel buffers created through the
//! display syscall as buffer slots, and presents damage rectangles
//! ([`frames`]). Input arrives as one-way messages on the event endpoint;
//! pointer coordinates are surface-relative and presses carry the button id.

use std::rc::Rc;

use libmessenger::Parcel;
use messenger_generated::os_lazy_display_v1 as wire;

use crate::platform::messenger::{client, Observed};
use crate::sys::errno;

pub(crate) mod cursor;
mod dnd;
mod frames;
mod shell;

pub use dnd::{decode_drag_event, DragEvent};
pub use frames::{decode_frame_event, FrameEvent};
pub use shell::{decode_shell_event, ShellEvent, SurfaceChange};

/// Interface id every display parcel carries.
pub const INTERFACE: u64 = wire::INTERFACE_ID;
/// Method id of the one-way `WindowClose` event.
pub const METHOD_WINDOW_CLOSE: u32 = wire::METHOD_WINDOWCLOSE;

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
    /// The wheel rolled `delta` notches at `(x, y)`; positive scrolls up.
    PointerWheel { x: i32, y: i32, delta: i32 },
    /// A key went down; `key` is the kernel key code.
    KeyDown { key: u32 },
    /// A key was released.
    KeyUp { key: u32 },
    /// The window manager changed the surface's content size: the app must
    /// attach a new-size buffer and repaint. `state` is a
    /// `wire::WINDOW_STATE_*`.
    Configure { width: i32, height: i32, state: u32 },
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
        wire::METHOD_POINTERWHEEL => {
            let args = wire::decode_pointer_wheel_args(body).ok()?;
            Event::PointerWheel {
                x: args.x,
                y: args.y,
                delta: args.delta,
            }
        }
        wire::METHOD_KEYDOWN => Event::KeyDown {
            key: wire::decode_key_down_args(body).ok()?.key,
        },
        wire::METHOD_KEYUP => Event::KeyUp {
            key: wire::decode_key_up_args(body).ok()?.key,
        },
        wire::METHOD_CONFIGURE => {
            let args = wire::decode_configure_args(body).ok()?;
            Event::Configure {
                width: i32::try_from(args.width).unwrap_or(i32::MAX),
                height: i32::try_from(args.height).unwrap_or(i32::MAX),
                state: args.state,
            }
        }
        _ => return None,
    })
}

/// The generated client of the compositor.
type Display = wire::Client<Observed>;

/// An app's connection to `xuid`: a handle on this thread's generated client
/// ([`client`]), so it is `Copy` and every window of the thread shares one
/// resolved endpoint.
#[derive(Clone, Copy, Debug)]
pub struct Client {
    /// A client that reaches nothing, for host tests of the backend's
    /// bookkeeping.
    #[cfg(test)]
    detached: bool,
}

impl Client {
    /// A connection that reaches nothing, for host tests of the backend's
    /// bookkeeping.
    #[cfg(test)]
    pub(crate) fn detached() -> Client {
        Client { detached: true }
    }

    /// Resolve the compositor into this thread, waiting for it to start
    /// (`xuid` is spawned before the app, but a slow load of its binary makes
    /// the race one-sided).
    pub fn connect() -> Result<Client, i64> {
        client(wire::Client::connect)?;
        Ok(Client {
            #[cfg(test)]
            detached: false,
        })
    }

    /// This thread's generated client. A compositor that restarted is
    /// resolved again by the call itself.
    fn wire(&self) -> Result<Rc<Display>, i64> {
        Ok(client(wire::Client::connect)?)
    }

    /// `CreateSurface(width, height, title, events)` for a decorated window;
    /// the event endpoint handle is moved to the compositor, which sends input
    /// back on it. The reply carries the new surface id.
    pub fn create_surface(
        &self,
        width: u64,
        height: u64,
        title: &str,
        events: u64,
    ) -> Result<u64, i64> {
        self.create_surface_with_role(width, height, title, events, wire::ROLE_WINDOW)
    }

    /// [`Client::create_surface`] with an explicit `Role` (`wire::ROLE_*`):
    /// the desktop and panel roles are for the task holding the `shell`
    /// subscription, and the compositor refuses them from anyone else.
    pub fn create_surface_with_role(
        &self,
        width: u64,
        height: u64,
        title: &str,
        events: u64,
        role: u32,
    ) -> Result<u64, i64> {
        let surface = self.wire()?.create_surface(
            u32::try_from(width).unwrap_or(u32::MAX),
            u32::try_from(height).unwrap_or(u32::MAX),
            title,
            role,
            None,
            events,
        )?;
        // Surface ids start at 1; zero is a missing field.
        if surface == 0 {
            return Err(-errno::EINVAL);
        }
        Ok(surface)
    }

    /// `SetTitle`: rename `surface`'s window. An older compositor answers
    /// `EINVAL` (the method is unknown to it), which callers ignore: the title
    /// only decorates.
    pub fn set_title(&self, surface: u64, title: &str) -> Result<(), i64> {
        Ok(self.wire()?.set_title(surface, title)?)
    }

    /// `SetCloseMode`: keep `surface`'s window when the window manager closes
    /// it, until the app destroys the surface (so it can ask first). An older
    /// compositor answers `EINVAL`, which callers ignore: the window then
    /// closes as before.
    pub fn set_close_mode(&self, surface: u64, ask: bool) -> Result<(), i64> {
        Ok(self.wire()?.set_close_mode(surface, ask)?)
    }

    /// `SetSizeHints`: declare `surface` resizable within the given content
    /// bounds (a `max` of 0 means the screen). An older compositor answers
    /// `EINVAL`; callers may ignore it, leaving the window fixed-size.
    pub fn set_size_hints(
        &self,
        surface: u64,
        min_w: u32,
        min_h: u32,
        max_w: u32,
        max_h: u32,
    ) -> Result<(), i64> {
        Ok(self
            .wire()?
            .set_size_hints(surface, min_w, min_h, max_w, max_h)?)
    }

    /// `RequestSize`: ask the compositor to resize `surface`'s content (a
    /// compact/expanded toggle). The answer is a `Configure` event with the
    /// size it applied; an older compositor or a window without size hints
    /// answers `EINVAL`, which callers ignore.
    pub fn request_size(&self, surface: u64, width: u32, height: u32) -> Result<(), i64> {
        Ok(self.wire()?.request_size(surface, width, height)?)
    }

    /// `HintOpenOrigin`: ask the compositor to open this task's next window
    /// from `rect` (`x, y, w, h`, relative to `surface`'s content origin)
    /// instead of from its taskbar entry. Purely cosmetic: an older
    /// compositor answers `EINVAL`, and an unresponsive one times out after
    /// half a second (the `.midl` deadline); callers ignore both and go on to
    /// create the window.
    pub fn hint_open_origin(&self, surface: u64, rect: (i32, i32, u32, u32)) -> Result<(), i64> {
        Ok(self
            .wire()?
            .hint_open_origin(surface, rect.0, rect.1, rect.2, rect.3)?)
    }

    /// `GetTheme`: the desktop's mode and accent (and chrome palette). The
    /// compositor answers it from memory, but while several apps start at once
    /// its queue can hold the reply past a short bound, and a window painted
    /// in the wrong mode is worse than a short wait (issue #542), so it waits
    /// the default five seconds, not less, and never for ever.
    pub fn get_theme(&self) -> Result<wire::GetThemeReply, i64> {
        Ok(self.wire()?.get_theme()?)
    }

    /// `GetOutput`: the screen size and the desktop's UI scale
    /// (docs/hidpi-plan.md). An older compositor answers `EINVAL` and the
    /// caller assumes scale 1.
    pub fn get_output(&self) -> Result<wire::GetOutputReply, i64> {
        Ok(self.wire()?.get_output()?)
    }

    /// Drop `surface`; the compositor forgets it and repaints.
    pub fn destroy_surface(&self, surface: u64) -> Result<(), i64> {
        Ok(self.wire()?.destroy_surface(surface)?)
    }
}

/// A parcel as the compositor sends an event: for host tests that decode one.
#[cfg(test)]
fn request(method: u32, body: Vec<u8>, objects: Vec<libmessenger::Object>) -> Parcel {
    Parcel {
        header: libmessenger::Header {
            version: libmessenger::VERSION,
            flags: libmessenger::flags::ALLOW_NESTED,
            interface_id: INTERFACE,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        objects,
    }
}

/// Decode a whole received message: its method and the parcel. Returns `None`
/// when the bytes are not a parcel.
pub fn decode_message(bytes: &[u8]) -> Option<Parcel> {
    Parcel::decode(bytes).ok()
}

/// Close a handle opened by [`Client::connect`].
pub use lazyos_sys::msg::close;

/// Release this task's handle to an endpoint, closing its side only when no
/// other handle names it. A handle from `msg_resolve` must be released, not
/// closed: every client of a service shares that side, and closing it would
/// make the service's own endpoint report a closed peer for good (a service
/// parked on it would then spin).
pub use lazyos_sys::msg::release;

#[cfg(test)]
mod tests;
