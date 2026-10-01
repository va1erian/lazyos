//! Input/drag/shell event decode and send over the generated
//! `os.lazy.display.v1` stubs, plus [`Theme`] and [`SurfaceInfo`] (the
//! `GetTheme`/`ListSurfaces` reply shapes).

use alloc::string::String;
use alloc::vec::Vec;

use libmessenger::{flags, Parcel};

use super::super::endpoint::syscall;
use super::super::{op, Endpoint, Message, MsgArgs, MsgResult, Result};
use super::canvas::Color;
use super::{wire, INTERFACE};

/// One row of a `ListSurfaces` reply (issue #167): id, title, geometry,
/// minimized/focused flags and a [`wire::ROLE_WINDOW`]/[`wire::ROLE_DESKTOP`]
/// role.
pub use wire::SurfaceRow as SurfaceInfo;

/// One decoded input event, in the surface's own coordinates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// The pointer moved; `(x, y)` is relative to the surface content and may
    /// lie outside it during a press-and-drag.
    PointerMove { x: i32, y: i32 },
    /// A pointer button went down; `button` is a [`super::button`] id.
    PointerDown { x: i32, y: i32, button: u32 },
    /// A pointer button went up.
    PointerUp { x: i32, y: i32, button: u32 },
    /// The wheel rolled `delta` notches over the surface at `(x, y)`; positive
    /// scrolls up.
    PointerWheel { x: i32, y: i32, delta: i32 },
    /// A key went down; `key` is a character or a [`super::key`] code.
    KeyDown { key: u32 },
    /// A key was released.
    KeyUp { key: u32 },
    /// The window manager resized the surface's content to `width` x `height`
    /// (`state` is a `wire::WINDOW_STATE_*` value). Only surfaces that called
    /// [`super::Client::set_size_hints`] receive it; the client should attach
    /// a buffer of the new size and redraw, and until it does the compositor
    /// shows the old buffer cropped or padded.
    Configure { width: u32, height: u32, state: u32 },
}

/// Decode an input event from a received message, or `None` when the
/// message is not a (well-formed) display input event.
pub fn decode_event(message: &Message) -> Option<Event> {
    let body = &message.parcel.body;
    Some(match message.method() {
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
                width: args.width,
                height: args.height,
                state: args.state,
            }
        }
        _ => return None,
    })
}

/// A frame-pacing event for a surface that uses `Present` (issue #361).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameEvent {
    /// The compositor no longer reads `slot`; the client may draw into it.
    BufferRelease { surface: u64, slot: u32 },
    /// The compositor consumed the `Present` numbered `seq`.
    FrameDone { surface: u64, seq: u64 },
}

/// Decode a [`FrameEvent`] from a received message, or `None` when the
/// message is not one. [`decode_event`] still handles input events.
pub fn decode_frame_event(message: &Message) -> Option<FrameEvent> {
    let body = &message.parcel.body;
    match message.method() {
        wire::METHOD_BUFFERRELEASE => {
            let args = wire::decode_buffer_release_args(body).ok()?;
            Some(FrameEvent::BufferRelease {
                surface: args.surface,
                slot: args.slot,
            })
        }
        wire::METHOD_FRAMEDONE => {
            let args = wire::decode_frame_done_args(body).ok()?;
            Some(FrameEvent::FrameDone {
                surface: args.surface,
                seq: args.seq,
            })
        }
        _ => None,
    }
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
/// drop; `mime` is set on enter and drop, `token` on a drop; `dropped` is set
/// on an ended.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DragEvent {
    pub kind: DragKind,
    pub x: i32,
    pub y: i32,
    pub token: u64,
    pub mime: String,
    pub dropped: bool,
}

/// Decode a drag event from a received message, or `None` when the message
/// is not one. [`decode_event`] still handles input events.
pub fn decode_drag_event(message: &Message) -> Option<DragEvent> {
    let body = &message.parcel.body;
    let mut event = DragEvent {
        kind: DragKind::Leave,
        x: 0,
        y: 0,
        token: 0,
        mime: String::new(),
        dropped: false,
    };
    match message.method() {
        wire::METHOD_DRAGENTER => {
            let args = wire::decode_drag_enter_args(body).ok()?;
            event.kind = DragKind::Enter;
            (event.x, event.y, event.mime) = (args.x, args.y, args.mime);
        }
        wire::METHOD_DRAGOVER => {
            let args = wire::decode_drag_over_args(body).ok()?;
            event.kind = DragKind::Over;
            (event.x, event.y) = (args.x, args.y);
        }
        wire::METHOD_DRAGLEAVE => {}
        wire::METHOD_DROP => {
            let args = wire::decode_drop_args(body).ok()?;
            event.kind = DragKind::Drop;
            (event.x, event.y) = (args.x, args.y);
            (event.token, event.mime) = (args.token, args.mime);
        }
        wire::METHOD_DRAGENDED => {
            event.kind = DragKind::Ended;
            event.dropped = wire::decode_drag_ended_args(body).ok()?.dropped;
        }
        _ => return None,
    }
    Some(event)
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
    /// Text on an inactive title bar.
    pub text: Color,
    /// Whether the desktop uses the light preset (`sys/ui/mode`); a
    /// compositor that predates the field reports dark.
    pub light: bool,
    /// The accent colour in effect.
    pub accent: Color,
}

impl Theme {
    /// The palette a `GetTheme` reply describes.
    pub(super) fn from_reply(reply: &wire::GetThemeReply) -> Theme {
        Theme {
            title_bg_active: color_from_u32(reply.title_bg_active),
            title_bg_inactive: color_from_u32(reply.title_bg_inactive),
            border: color_from_u32(reply.border),
            taskbar: color_from_u32(reply.taskbar),
            text: color_from_u32(reply.text),
            light: reply.mode == "light",
            accent: color_from_u32(reply.accent),
        }
    }
}

/// Unpack a `0xRRGGBB` theme colour.
fn color_from_u32(value: u32) -> Color {
    Color::rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

/// One `SurfaceChanged` event (issue #167); `kind` is a `wire::CHANGE_*`.
pub type SurfaceChanged = wire::SurfaceChangedArgs;

/// A one-way event for the shell subscriber (issue #167).
#[derive(Clone, PartialEq, Debug)]
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
    let body = &message.parcel.body;
    match message.method() {
        wire::METHOD_SURFACECHANGED => Some(ShellEvent::SurfaceChanged(
            wire::decode_surface_changed_args(body).ok()?,
        )),
        wire::METHOD_FOCUSCHANGED => {
            let args = wire::decode_focus_changed_args(body).ok()?;
            Some(ShellEvent::FocusChanged(args.surface))
        }
        wire::METHOD_STARTMENU => Some(ShellEvent::StartMenu),
        _ => None,
    }
}

/// Send one one-way event of `method` with the encoded `body` to `endpoint`.
///
/// `scratch` is the reusable parcel-encode buffer: the compositor sends
/// events at input rates and keeps one buffer for all of them.
pub fn send_event(
    endpoint: &Endpoint,
    scratch: &mut Vec<u8>,
    method: u32,
    body: core::result::Result<Vec<u8>, libmessenger::Error>,
) -> Result<()> {
    let mut parcel = Parcel::default();
    parcel.header.version = libmessenger::VERSION;
    parcel.header.flags = flags::ONE_WAY;
    parcel.header.interface_id = INTERFACE;
    parcel.header.method = method;
    parcel.body = body.map_err(super::super::Error::Parcel)?;
    parcel
        .encode(scratch)
        .map_err(super::super::Error::Parcel)?;
    let args = MsgArgs {
        handle: endpoint.handle(),
        parcel_ptr: scratch.as_ptr() as u64,
        parcel_len: scratch.len() as u64,
        ..MsgArgs::default()
    };
    syscall(op::SEND, &args, &mut MsgResult::default())
}
