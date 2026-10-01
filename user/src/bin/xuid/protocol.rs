//! Display protocol helpers and kernel event decoding (issue #194 split): the
//! reply builders, the raw kernel input record decoder and the privilege
//! check. The wire itself is the generated `display::wire` (issue #287).

use alloc::vec::Vec;
use libmessenger::Parcel;
use user::messenger::display::{self, Color};
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

/// `sender`'s kernel-stamped credentials, or `None` when they cannot be read
/// (which every caller treats as "not authorized").
pub(super) fn sender_cred(sender: u64) -> Option<sys::Cred> {
    let mut cred = sys::Cred::default();
    sys::cred_get(Some(sender), &mut cred).ok().map(|()| cred)
}

/// Whether `cred` authorizes the compositor's administrative operations
/// (issue #175) on its own: any subscription role, the shell-only calls.
/// Mirrors accountsd's admin check: uid 0, or `CAP_SETUID` for a delegated
/// system service.
pub(super) fn privileged(cred: &sys::Cred) -> bool {
    cred.uid == 0 || cred.caps & sys::CAP_SETUID != 0
}

/// [`privileged`] for `sender`; a read error is "not authorized".
pub(super) fn is_privileged(sender: u64) -> bool {
    sender_cred(sender).is_some_and(|cred| privileged(&cred))
}
/// The kind of a raw kernel input record.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum EventKind {
    PointerMove,
    PointerDown,
    PointerUp,
    KeyDown,
    KeyUp,
    PointerWheel,
}

impl EventKind {
    /// Whether this is a pointer record (not a key).
    pub(super) fn is_pointer(self) -> bool {
        !matches!(self, EventKind::KeyDown | EventKind::KeyUp)
    }
}

/// One raw kernel input record: `a`/`b` carry the pointer `(x, y)`, the button
/// id, the wheel notches (`a`) or the key code depending on the kind. Screen-absolute; `xuid`
/// translates to surface coordinates before forwarding.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Event {
    pub kind: EventKind,
    pub a: i64,
    pub b: i64,
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
        raw_kind::POINTER_WHEEL => EventKind::PointerWheel,
        _ => return None,
    };
    Some(Event {
        kind,
        a: a as i64,
        b: b as i64,
    })
}

/// Append `event` to an input batch, collapsing a run of pointer moves into
/// its last record. Moves carry the absolute pointer position, so only the
/// final one of a run matters: every move `xuid` handles costs a forwarded
/// message to the focused surface plus a cursor repaint (issue #339). A
/// press, release or key between two moves ends the run, so it still sees
/// the position that preceded it.
pub(super) fn push_coalesced(batch: &mut Vec<Event>, event: Event) {
    if event.kind == EventKind::PointerMove {
        if let Some(last) = batch.last_mut() {
            if last.kind == EventKind::PointerMove {
                *last = event;
                return;
            }
        }
    }
    batch.push(event);
}

/// The kernel's event kind constants; the user mirror exposes the protocol
/// methods, not these raw codes, so they are repeated here.
mod raw_kind {
    pub const POINTER_MOVE: u32 = 0;
    pub const POINTER_DOWN: u32 = 1;
    pub const POINTER_UP: u32 = 2;
    pub const KEY_DOWN: u32 = 3;
    pub const KEY_UP: u32 = 4;
    pub const POINTER_WHEEL: u32 = 5;
}
/// Pack a colour into the `0xRRGGBB` form `GetTheme` reports.
pub(super) fn color_u32(color: Color) -> u32 {
    ((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32
}

/// An empty reply carrying only the header.
pub(super) fn empty_reply(method: u32) -> Parcel {
    display::reply(method, Vec::new())
}

/// An error reply: an `Error` TLV with a positive code, as the daemon
/// convention in this codebase uses.
pub(super) fn error_reply(method: u32, code: i64) -> Parcel {
    display::error_reply(method, code)
}

/// A success reply carrying an encoded generated `body`; an encode failure
/// (an oversized body) is reported as `EINVAL` rather than a silent empty one.
pub(super) fn typed_reply(method: u32, body: Result<Vec<u8>, libmessenger::Error>) -> Parcel {
    match body {
        Ok(body) => display::reply(method, body),
        Err(_) => error_reply(method, user::messenger::errno::EINVAL),
    }
}
