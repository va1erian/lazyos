//! Display protocol wire helpers and event decoding (issue #194 split): the
//! method ids, TLV field lookups, reply builders, the kernel event decoder and
//! the privilege check, moved out of `xuid.rs` unchanged.

use alloc::string::String;
use alloc::vec::Vec;
use libmessenger::{Decoder, Encoder, Kind, Parcel, VERSION};
use user::messenger::display::{self, Color, Event, EventKind};
use user::messenger::{Endpoint, Message};
use user::sys;

/// Close the endpoint handle that arrived with a request we are rejecting;
/// otherwise every refused request leaks one slot in the compositor's
/// (immortal) handle table.
pub(super) fn drop_rejected_handle(message: &Message) {
    if message.handles != 0 {
        let _ = Endpoint::from_raw(message.first_handle).close();
    }
}

/// Whether `sender`'s kernel-stamped credentials authorize the compositor's
/// administrative operations (issue #175): claiming the `"shell"` role,
/// replacing the desktop, and listing every surface. Mirrors accountsd's
/// admin check: uid 0, or `CAP_SETUID` for a delegated system service. A
/// refusal or a read error is "not authorized".
pub(super) fn is_privileged(sender: u64) -> bool {
    let mut cred = sys::Cred::default();
    match sys::cred_get(Some(sender), &mut cred) {
        Ok(()) => cred.uid == 0 || cred.caps & sys::CAP_SETUID != 0,
        Err(_) => false,
    }
}
/// Decode the `index`-th 16-byte kernel event record.
pub(super) fn decode_event(bytes: &[u8], index: usize) -> Option<Event> {
    let base = index * 16;
    if base + 16 > bytes.len() {
        return None;
    }
    let word = |at: usize| -> u32 {
        u32::from_le_bytes([
            bytes[base + at],
            bytes[base + at + 1],
            bytes[base + at + 2],
            bytes[base + at + 3],
        ])
    };
    let (kind, a, b) = (word(0), word(4) as i32, word(8) as i32);
    let kind = match kind {
        raw_kind::POINTER_MOVE => EventKind::PointerMove,
        raw_kind::POINTER_DOWN => EventKind::PointerDown,
        raw_kind::POINTER_UP => EventKind::PointerUp,
        raw_kind::KEY_DOWN => EventKind::KeyDown,
        raw_kind::KEY_UP => EventKind::KeyUp,
        _ => return None,
    };
    Some(Event {
        kind,
        a: a as i64,
        b: b as i64,
    })
}

/// The kernel's event kind constants; the user mirror exposes the protocol
/// methods, not these raw codes, so they are repeated here.
mod raw_kind {
    pub const POINTER_MOVE: u32 = 0;
    pub const POINTER_DOWN: u32 = 1;
    pub const POINTER_UP: u32 = 2;
    pub const KEY_DOWN: u32 = 3;
    pub const KEY_UP: u32 = 4;
}
/// Pack a colour into the `0xRRGGBB` form `GetTheme` reports.
pub(super) fn color_u64(color: Color) -> u64 {
    ((color.r as u64) << 16) | ((color.g as u64) << 8) | color.b as u64
}

/// The protocol method ids. (The client mirror in `user::messenger::display`
/// has the same values; a compositor binary is not generic over them.)
pub(super) mod method {
    pub const CREATE_SURFACE: u32 = 1;
    pub const ATTACH_BUFFER: u32 = 2;
    pub const COMMIT: u32 = 3;
    pub const DESTROY_SURFACE: u32 = 4;
    pub const POINTER_MOVE: u32 = 5;
    pub const POINTER_DOWN: u32 = 6;
    pub const POINTER_UP: u32 = 7;
    pub const KEY_DOWN: u32 = 8;
    pub const KEY_UP: u32 = 9;
    pub const WINDOW_CLOSE: u32 = 10;
    pub const DRAG_START: u32 = 11;
    pub const DRAG_CANCEL: u32 = 12;
    pub const DRAG_ENTER: u32 = 13;
    pub const DRAG_OVER: u32 = 14;
    pub const DRAG_LEAVE: u32 = 15;
    pub const DROP: u32 = 16;
    pub const DRAG_ENDED: u32 = 17;
    pub const LIST_SURFACES: u32 = 18;
    pub const GET_WORK_AREA: u32 = 19;
    pub const SUBSCRIBE: u32 = 20;
    pub const GET_THEME: u32 = 21;
    pub const SURFACE_CHANGED: u32 = 22;
    pub const FOCUS_CHANGED: u32 = 23;
    pub const START_MENU: u32 = 24;
}

/// Find the first `u64` field with `id`.
pub(super) fn u64_field(parcel: &Parcel, id: u16) -> Option<u64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::U64 && field.id == id {
            return field.as_u64().ok();
        }
    }
    None
}

/// Find the first string field with `id`.
pub(super) fn string_field(parcel: &Parcel, id: u16) -> Option<String> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::String && field.id == id {
            return field.as_str().ok().map(String::from);
        }
    }
    None
}

/// An empty reply carrying only the header.
pub(super) fn empty_reply(method: u32) -> Parcel {
    reply_parcel(method, Encoder::new())
}

/// An error reply: an `Error` TLV with a positive code, as the daemon
/// convention in this codebase uses.
pub(super) fn error_reply(method: u32, code: i64) -> Parcel {
    let mut body = Encoder::new();
    let _ = body.error(display::field::ERROR, code as u32, "display request failed");
    reply_parcel(method, body)
}

/// Build a reply parcel for `method`.
pub(super) fn reply_parcel(method: u32, body: Encoder) -> Parcel {
    Parcel {
        header: libmessenger::Header {
            version: VERSION,
            flags: 0,
            interface_id: display::INTERFACE,
            method,
            txn_id: 0,
            reply_to: 0,
            deadline_ns: 0,
        },
        body: body.finish(),
        handles: Vec::new(),
        buffers: Vec::new(),
    }
}
