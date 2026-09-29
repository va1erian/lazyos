//! The display protocol client (issue #113). See the module doc on
//! [`crate::messenger::display`] for the client/compositor/rendering model.
//!
//! Split into [`client`] ([`Client`], the app's connection to the
//! compositor), [`events`] (input/drag/shell event decode and encode, plus
//! [`Theme`]/[`SurfaceInfo`]), and [`canvas`] (the software blitter: [`Rect`],
//! [`Color`], [`Canvas`], [`font`]); all three are re-exported here so callers
//! keep using `display::*`.

use alloc::vec::Vec;

use libmessenger::{flags, BufferDesc, Decoder, Encoder, Header, Kind, Parcel, VERSION};

mod canvas;
mod client;
mod events;

pub use canvas::*;
pub use client::*;
pub use events::*;

/// The generated `os.lazy.display.v1` stubs (see `idl/display.midl`): method
/// ids, argument/reply records and their codecs.
pub use messenger_generated::os_lazy_display_v1 as wire;

/// Well-known compositor name.
pub const NAME: &str = "os.lazy.display.v1";
/// Interface id every display parcel carries: the hash of [`NAME`].
pub const INTERFACE: u64 = wire::INTERFACE_ID;

/// Structured error field id in a failure reply. The generated fields of any
/// message use ids 1..=10, so this never collides with a success payload.
pub const ERROR_FIELD: u16 = 15;

/// The subscriber role that asks xuid to hide its built-in taskbar
/// (issue #167).
pub const ROLE_SHELL: &str = "shell";

/// Longest MIME string the compositor accepts in a `DragStart`.
pub const MAX_MIME: usize = 64;

/// Longest subscriber role string the compositor accepts in `Subscribe`.
pub const MAX_ROLE: usize = 32;

/// Key codes for non-character keys; mirrors `kernel/src/display.rs`.
pub mod key {
    pub const ENTER: u32 = 13;
    pub const BACKSPACE: u32 = 8;
    pub const TAB: u32 = 9;
    pub const ESCAPE: u32 = 27;
    pub const SPACE: u32 = 32;
    pub const LEFT: u32 = 0x100;
    pub const RIGHT: u32 = 0x101;
    pub const UP: u32 = 0x102;
    pub const DOWN: u32 = 0x103;
    pub const PAGE_UP: u32 = 0x104;
    pub const PAGE_DOWN: u32 = 0x105;
    pub const HOME: u32 = 0x106;
    pub const END: u32 = 0x107;
    /// Modifier keys (issue #167). The compositor consumes them for global
    /// hotkeys and never forwards them to a client; clients that forward
    /// raw input may still decode them defensively.
    pub const SHIFT: u32 = 0x108;
    pub const CTRL: u32 = 0x109;
    pub const ALT: u32 = 0x10A;
    pub const SUPER: u32 = 0x10B;
    /// Function key 4, used for the compositor's Alt+F4 (issue #167).
    pub const F4: u32 = 0x113;
}

/// Pointer buttons, as reported in pointer events.
pub mod button {
    pub const LEFT: u32 = 1;
    pub const RIGHT: u32 = 2;
    pub const MIDDLE: u32 = 3;
}

/// PIT ticks `Client::connect` waits for the compositor's name to appear.
/// The kernel spawns `xuid` before its demo client, but the compositor must
/// still bind the display and register the name, so a short retry window
/// keeps the app robust to that race.
const CONNECT_TICKS: u64 = 100;

/// A request parcel of `method` carrying an encoded `body`. `ALLOW_NESTED`
/// keeps an app's event poll from tripping the kernel's per-channel cycle
/// check while a `Commit` call is in flight.
fn request(method: u32, body: Vec<u8>, handles: Vec<u64>, buffers: Vec<BufferDesc>) -> Parcel {
    Parcel {
        header: Header {
            version: VERSION,
            flags: flags::ALLOW_NESTED,
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

/// A reply parcel for `method`; also the compositor's reply builder.
pub fn reply(method: u32, body: Vec<u8>) -> Parcel {
    Parcel {
        header: Header {
            version: VERSION,
            flags: 0,
            interface_id: INTERFACE,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body,
        handles: Vec::new(),
        buffers: Vec::new(),
    }
}

/// A failure reply: the structured [`ERROR_FIELD`] with a positive errno.
pub fn error_reply(method: u32, code: i64) -> Parcel {
    let mut body = Encoder::new();
    // A structured error field cannot overflow a fresh encoder.
    let _ = body.error(ERROR_FIELD, code as u32, "display request failed");
    reply(method, body.finish())
}

/// The positive errno in a failure reply, when the compositor refused a call.
fn error_field(parcel: &Parcel) -> Option<i64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::Error && field.id == ERROR_FIELD {
            let (code, _message) = field.error_parts().ok()?;
            return Some(code as i64);
        }
    }
    None
}
