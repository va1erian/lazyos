//! Input/drag/shell event decode and encode, plus [`Theme`] and
//! [`SurfaceInfo`] (the `ListSurfaces`/`GetTheme` reply shapes).

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{flags, Decoder, Kind, VERSION};

use super::super::endpoint::syscall;
use super::super::{op, Endpoint, Error, Message, MsgArgs, MsgResult, Result};
use super::canvas::Color;
use super::{field, method, role, INTERFACE};

/// The kind of an input event delivered to an app.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EventKind {
    PointerMove,
    PointerDown,
    PointerUp,
    KeyDown,
    KeyUp,
}

/// One decoded input event. `a`/`b` carry: pointer `(x, y)`, button id, or
/// key code, depending on the kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Event {
    pub kind: EventKind,
    pub a: i64,
    pub b: i64,
}

/// Decode an input event from a received message, or `None` when the
/// message is not a display event.
pub fn decode_event(message: &Message) -> Option<Event> {
    let kind = match message.method() {
        method::POINTER_MOVE => EventKind::PointerMove,
        method::POINTER_DOWN => EventKind::PointerDown,
        method::POINTER_UP => EventKind::PointerUp,
        method::KEY_DOWN => EventKind::KeyDown,
        method::KEY_UP => EventKind::KeyUp,
        _ => return None,
    };
    let mut a = 0i64;
    let mut b = 0i64;
    let mut decoder = Decoder::new(&message.parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind != Kind::U64 {
            continue;
        }
        match field.id {
            self::field::A => a = field.as_u64().ok()? as i64,
            self::field::B => b = field.as_u64().ok()? as i64,
            _ => {}
        }
    }
    Some(Event { kind, a, b })
}

/// The kind of a drag event the compositor delivers (issue #145).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DragKind {
    /// The drag entered this surface.
    Enter,
    /// The drag moved inside this surface.
    Over,
    /// The drag left this surface.
    Leave,
    /// The drag was released over this surface.
    Drop,
    /// (Source only) the drag ended: dropped or cancelled.
    Ended,
}

/// One decoded drag event. `x`/`y` are surface-relative for enter, over and
/// drop; `token`/`mime` are set on a drop; `dropped` is set on an ended.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DragEvent {
    pub kind: DragKind,
    pub x: i64,
    pub y: i64,
    pub token: u64,
    pub mime: String,
    pub dropped: bool,
}

/// Decode a drag event from a received message, or `None` when the message
/// is not one. [`decode_event`] still handles input events.
pub fn decode_drag_event(message: &Message) -> Option<DragEvent> {
    let kind = match message.method() {
        method::DRAG_ENTER => DragKind::Enter,
        method::DRAG_OVER => DragKind::Over,
        method::DRAG_LEAVE => DragKind::Leave,
        method::DROP => DragKind::Drop,
        method::DRAG_ENDED => DragKind::Ended,
        _ => return None,
    };
    let mut event = DragEvent {
        kind,
        x: 0,
        y: 0,
        token: 0,
        mime: String::new(),
        dropped: false,
    };
    let mut decoder = Decoder::new(&message.parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        match (field.kind, field.id) {
            (Kind::U64, self::field::A) => event.x = field.as_u64().ok()? as i64,
            (Kind::U64, self::field::B) => event.y = field.as_u64().ok()? as i64,
            (Kind::U64, self::field::TOKEN) => event.token = field.as_u64().ok()?,
            (Kind::String, self::field::MIME) => {
                event.mime = String::from(field.as_str().ok()?);
            }
            _ => {}
        }
    }
    if event.kind == DragKind::Ended {
        event.dropped = event.x != 0;
    }
    Some(event)
}

/// One row of a `ListSurfaces` reply (issue #167).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SurfaceInfo {
    /// Protocol surface id.
    pub id: u64,
    /// Window title from `CreateSurface`.
    pub title: String,
    /// Window origin (the decorated window's top-left for a window; the
    /// surface origin for a desktop).
    pub x: i32,
    pub y: i32,
    /// Window content size in pixels.
    pub w: i32,
    pub h: i32,
    /// Hidden by the minimize button.
    pub minimized: bool,
    /// The compositor's focused surface.
    pub focused: bool,
    /// One of [`role`] (issue #175): lets a shell tell the desktop from a
    /// window.
    pub role: u64,
}

/// Decode a `ListSurfaces` reply body into rows. Rows are delimited by the
/// `SURFACE` field, so unknown fields between rows are ignored and a newer
/// compositor can add fields without breaking this decoder.
pub fn decode_surface_list(body: &[u8]) -> Result<Vec<SurfaceInfo>> {
    let mut rows: Vec<SurfaceInfo> = Vec::new();
    let mut current: Option<SurfaceInfo> = None;
    let mut decoder = Decoder::new(body);
    while let Some(field) = decoder.next().map_err(Error::Parcel)? {
        match (field.kind, field.id) {
            (Kind::U64, self::field::SURFACE) => {
                if let Some(row) = current.take() {
                    rows.push(row);
                }
                current = Some(SurfaceInfo {
                    id: field.as_u64().map_err(Error::Parcel)?,
                    title: String::new(),
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 0,
                    minimized: false,
                    focused: false,
                    role: role::WINDOW,
                });
            }
            (Kind::String, self::field::TITLE) => {
                if let Some(row) = current.as_mut() {
                    row.title = String::from(field.as_str().map_err(Error::Parcel)?);
                }
            }
            (Kind::U64, self::field::X) => {
                if let Some(row) = current.as_mut() {
                    row.x = field.as_u64().map_err(Error::Parcel)? as i32;
                }
            }
            (Kind::U64, self::field::Y) => {
                if let Some(row) = current.as_mut() {
                    row.y = field.as_u64().map_err(Error::Parcel)? as i32;
                }
            }
            (Kind::U64, self::field::W) => {
                if let Some(row) = current.as_mut() {
                    row.w = field.as_u64().map_err(Error::Parcel)? as i32;
                }
            }
            (Kind::U64, self::field::H) => {
                if let Some(row) = current.as_mut() {
                    row.h = field.as_u64().map_err(Error::Parcel)? as i32;
                }
            }
            (Kind::U64, self::field::MINIMIZED) => {
                if let Some(row) = current.as_mut() {
                    row.minimized = field.as_u64().map_err(Error::Parcel)? != 0;
                }
            }
            (Kind::U64, self::field::FOCUSED) => {
                if let Some(row) = current.as_mut() {
                    row.focused = field.as_u64().map_err(Error::Parcel)? != 0;
                }
            }
            (Kind::U64, self::field::ROLE) => {
                if let Some(row) = current.as_mut() {
                    row.role = field.as_u64().map_err(Error::Parcel)?;
                }
            }
            _ => {}
        }
    }
    if let Some(row) = current.take() {
        rows.push(row);
    }
    Ok(rows)
}

/// The compositor's chrome palette (`GetTheme`, issue #167).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Theme {
    /// Title bar of the focused window.
    pub title_bg_active: Color,
    /// Title bar of every other window.
    pub title_bg_inactive: Color,
    /// Window border.
    pub border: Color,
    /// Fallback taskbar strip.
    pub taskbar: Color,
    /// Chrome text (titles and taskbar entries).
    pub text: Color,
}

impl Default for Theme {
    fn default() -> Theme {
        Theme {
            title_bg_active: Color::rgb(0, 0, 0),
            title_bg_inactive: Color::rgb(0, 0, 0),
            border: Color::rgb(0, 0, 0),
            taskbar: Color::rgb(0, 0, 0),
            text: Color::rgb(0, 0, 0),
        }
    }
}

/// Unpack a `0xRRGGBB` theme colour.
///
/// `pub(super)` so [`super::client`] can decode `GetTheme` replies.
pub(super) fn color_from_u64(value: u64) -> Color {
    Color::rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

/// One `SurfaceChanged` event (issue #167).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SurfaceChanged {
    pub id: u64,
    /// One of [`super::change`].
    pub kind: u64,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub minimized: bool,
    pub focused: bool,
    /// Set on `CREATED` (the title never changes today; the `TITLE` kind
    /// carries it when a rename method lands).
    pub title: String,
    /// One of [`role`] (issue #175): lets a shell tell the desktop from a
    /// window.
    pub role: u64,
}

/// A one-way event for the shell subscriber (issue #167).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ShellEvent {
    /// A surface was created/destroyed/moved/minimized/restored.
    SurfaceChanged(SurfaceChanged),
    /// The focused surface changed; `None` when nothing is focused.
    FocusChanged(Option<u64>),
    /// The global start-menu hotkey (Ctrl+Esc or Super) fired.
    StartMenu,
}

/// Decode a shell event from a received message, or `None` when the
/// message is not one.
pub fn decode_shell_event(message: &Message) -> Option<ShellEvent> {
    match message.method() {
        method::SURFACE_CHANGED => {
            let mut event = SurfaceChanged {
                id: 0,
                kind: 0,
                x: 0,
                y: 0,
                w: 0,
                h: 0,
                minimized: false,
                focused: false,
                title: String::new(),
                role: role::WINDOW,
            };
            let mut decoder = Decoder::new(&message.parcel.body);
            while let Ok(Some(field)) = decoder.next() {
                match (field.kind, field.id) {
                    (Kind::U64, self::field::SURFACE) => event.id = field.as_u64().ok()?,
                    (Kind::U64, self::field::A) => event.kind = field.as_u64().ok()?,
                    (Kind::U64, self::field::X) => event.x = field.as_u64().ok()? as i32,
                    (Kind::U64, self::field::Y) => event.y = field.as_u64().ok()? as i32,
                    (Kind::U64, self::field::W) => event.w = field.as_u64().ok()? as i32,
                    (Kind::U64, self::field::H) => event.h = field.as_u64().ok()? as i32,
                    (Kind::U64, self::field::MINIMIZED) => {
                        event.minimized = field.as_u64().ok()? != 0;
                    }
                    (Kind::U64, self::field::FOCUSED) => {
                        event.focused = field.as_u64().ok()? != 0
                    }
                    (Kind::U64, self::field::ROLE) => event.role = field.as_u64().ok()?,
                    (Kind::String, self::field::TITLE) => {
                        event.title = String::from(field.as_str().ok()?);
                    }
                    _ => {}
                }
            }
            Some(ShellEvent::SurfaceChanged(event))
        }
        method::FOCUS_CHANGED => {
            let mut id = None;
            let mut decoder = Decoder::new(&message.parcel.body);
            while let Ok(Some(field)) = decoder.next() {
                if field.kind == Kind::U64 && field.id == self::field::SURFACE {
                    let value = field.as_u64().ok()?;
                    id = (value != 0).then_some(value);
                }
            }
            Some(ShellEvent::FocusChanged(id))
        }
        method::START_MENU => Some(ShellEvent::StartMenu),
        _ => None,
    }
}

/// Encode a one-way event parcel into `scratch`, replacing its contents.
///
/// The compositor sends events at input rates into a task whose bump
/// allocator never frees, so it cannot build a fresh `Parcel` per event.
/// The byte layout matches `libmessenger` exactly (header, `u64` TLV fields
/// in order, an optional string field, no handles or buffers).
pub fn encode_event_fields(
    scratch: &mut Vec<u8>,
    method: u32,
    fields: &[(u16, u64)],
    text: Option<(u16, &str)>,
) {
    scratch.clear();
    let text_len = text.map(|(_, value)| value.len()).unwrap_or(0);
    let body_len = fields.len() * 16 + if text.is_some() { 8 + text_len } else { 0 };
    scratch.extend_from_slice(&VERSION.to_le_bytes());
    scratch.extend_from_slice(&flags::ONE_WAY.to_le_bytes());
    scratch.extend_from_slice(&INTERFACE.to_le_bytes());
    scratch.extend_from_slice(&method.to_le_bytes());
    scratch.extend_from_slice(&0u64.to_le_bytes()); // txn_id
    scratch.extend_from_slice(&0u64.to_le_bytes()); // reply_to
    scratch.extend_from_slice(&0u64.to_le_bytes()); // deadline_ns
    scratch.extend_from_slice(&(body_len as u32).to_le_bytes());
    scratch.extend_from_slice(&0u16.to_le_bytes()); // handles
    scratch.extend_from_slice(&0u16.to_le_bytes()); // buffers
    for &(id, value) in fields {
        let tag = Kind::U64 as u32 | ((id as u32) << 8);
        scratch.extend_from_slice(&tag.to_le_bytes());
        scratch.extend_from_slice(&8u32.to_le_bytes());
        scratch.extend_from_slice(&value.to_le_bytes());
    }
    if let Some((id, value)) = text {
        let tag = Kind::String as u32 | ((id as u32) << 8);
        scratch.extend_from_slice(&tag.to_le_bytes());
        scratch.extend_from_slice(&(value.len() as u32).to_le_bytes());
        scratch.extend_from_slice(value.as_bytes());
    }
}

/// Encode an event with the two standard `A`/`B` fields.
pub fn encode_event(scratch: &mut Vec<u8>, method: u32, a: u64, b: u64) {
    encode_event_fields(scratch, method, &[(field::A, a), (field::B, b)], None);
}

/// Send one input event to `endpoint` using a reusable encode buffer.
pub fn send_event(
    endpoint: &Endpoint,
    scratch: &mut Vec<u8>,
    method: u32,
    a: u64,
    b: u64,
) -> Result<()> {
    encode_event(scratch, method, a, b);
    send_encoded(endpoint, scratch)
}

/// Send one event built by [`encode_event_fields`] to `endpoint`.
pub fn send_event_fields(
    endpoint: &Endpoint,
    scratch: &mut Vec<u8>,
    method: u32,
    fields: &[(u16, u64)],
    text: Option<(u16, &str)>,
) -> Result<()> {
    encode_event_fields(scratch, method, fields, text);
    send_encoded(endpoint, scratch)
}

/// Send the parcel bytes already encoded in `scratch`.
fn send_encoded(endpoint: &Endpoint, scratch: &[u8]) -> Result<()> {
    let args = MsgArgs {
        handle: endpoint.handle(),
        parcel_ptr: scratch.as_ptr() as u64,
        parcel_len: scratch.len() as u64,
        ..MsgArgs::default()
    };
    syscall(op::SEND, &args, &mut MsgResult::default())
}
