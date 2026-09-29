//! The display protocol client (issue #113). See the module doc on
//! [`crate::messenger::display`] for the client/compositor/rendering model.
//!
//! Split into [`client`] ([`Client`], the app's connection to the
//! compositor), [`events`] (input/drag/shell event decode and encode, plus
//! [`Theme`]/[`SurfaceInfo`]), and [`canvas`] (the software blitter: [`Rect`],
//! [`Color`], [`Canvas`], [`font`]); all three are re-exported here so callers
//! keep using `display::*`.

use libmessenger::{flags, Header, VERSION};

mod canvas;
mod client;
mod events;

pub use canvas::*;
pub use client::*;
pub use events::*;

/// Well-known compositor name. The interface id is the first eight bytes of
/// the same string, matching the kernel registry convention.
pub const NAME: &str = "os.lazy.display.v1";
/// Interface id (`os.lazy.` prefix, like the registry's).
pub const INTERFACE: u64 = u64::from_le_bytes(*b"os.lazy.");

/// Display protocol methods.
pub mod method {
    /// Create a surface; reply carries its id.
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
    /// Compositor to app: the window manager closed this surface (issue
    /// #143). One-way; the app is expected to exit (or re-create).
    pub const WINDOW_CLOSE: u32 = 10;
    /// App to compositor: begin a compositor-mediated drag carrying a
    /// clipboard token (issue #145).
    pub const DRAG_START: u32 = 11;
    /// App to compositor: cancel the drag that started at this surface.
    pub const DRAG_CANCEL: u32 = 12;
    /// Compositor to app: a drag entered this surface (`A`/`B` = local x/y).
    pub const DRAG_ENTER: u32 = 13;
    /// Compositor to app: a drag moved inside this surface (`A`/`B` = local
    /// x/y).
    pub const DRAG_OVER: u32 = 14;
    /// Compositor to app: a drag left this surface.
    pub const DRAG_LEAVE: u32 = 15;
    /// Compositor to app: a drag was released over this surface; carries
    /// `TOKEN` and `MIME`.
    pub const DROP: u32 = 16;
    /// Compositor to the source: the drag ended; `A` = 1 when dropped, 0
    /// when cancelled.
    pub const DRAG_ENDED: u32 = 17;
    /// List every surface; the reply is one row per surface (issue #167).
    pub const LIST_SURFACES: u32 = 18;
    /// The rectangle available to windows, above the fallback taskbar
    /// (issue #167).
    pub const GET_WORK_AREA: u32 = 19;
    /// Register this task as the shell subscriber; the parcel transfers an
    /// event endpoint (issue #167).
    pub const SUBSCRIBE: u32 = 20;
    /// The compositor's current chrome palette (issue #167).
    pub const GET_THEME: u32 = 21;
    /// Compositor to the shell: a surface was created, destroyed, moved,
    /// minimized, restored, or retitled (issue #167).
    pub const SURFACE_CHANGED: u32 = 22;
    /// Compositor to the shell: the focused surface changed (issue #167).
    pub const FOCUS_CHANGED: u32 = 23;
    /// Compositor to the shell: the global start-menu hotkey (Ctrl+Esc or
    /// Super) fired (issue #167).
    pub const START_MENU: u32 = 24;
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
    /// Event payload, first word (key code, pointer x, or button).
    pub const A: u16 = 9;
    /// Event payload, second word (pointer y).
    pub const B: u16 = 10;
    /// Structured error code in a failure reply.
    pub const ERROR: u16 = 11;
    /// Clipboard token a drag carries (issue #145).
    pub const TOKEN: u16 = 12;
    /// MIME type string of a drag payload.
    pub const MIME: u16 = 13;
    /// Surface role in `CreateSurface` (issue #167): [`role::WINDOW`] or
    /// [`role::DESKTOP`].
    pub const ROLE: u16 = 14;
    /// Surface minimized flag in list rows and change events (issue #167).
    pub const MINIMIZED: u16 = 15;
    /// Surface focused flag in list rows (issue #167).
    pub const FOCUSED: u16 = 16;
    /// Subscriber role string in `Subscribe` (issue #167).
    pub const SUBSCRIBER_ROLE: u16 = 17;
    /// Active title-bar colour in `GetTheme`, `0xRRGGBB` (issue #167).
    pub const TITLE_BG_ACTIVE: u16 = 18;
    /// Inactive title-bar colour in `GetTheme` (issue #167).
    pub const TITLE_BG_INACTIVE: u16 = 19;
    /// Window border colour in `GetTheme` (issue #167).
    pub const BORDER: u16 = 20;
    /// Taskbar colour in `GetTheme` (issue #167).
    pub const TASKBAR: u16 = 21;
    /// Chrome text colour in `GetTheme` (issue #167).
    pub const TEXT: u16 = 22;
}

/// Surface roles carried in the `CreateSurface` `ROLE` field (issue #167).
pub mod role {
    /// A regular decorated window (the default when the field is absent).
    pub const WINDOW: u64 = 0;
    /// The full-screen desktop surface, painted above the background and
    /// below every window; a new desktop replaces the current one.
    pub const DESKTOP: u64 = 1;
}

/// The `SURFACE_CHANGED` event kinds (issue #167).
pub mod change {
    pub const CREATED: u64 = 1;
    pub const DESTROYED: u64 = 2;
    pub const MOVED: u64 = 3;
    pub const MINIMIZED: u64 = 4;
    pub const RESTORED: u64 = 5;
    /// The surface's title changed (reserved; xuid has no rename method yet).
    pub const TITLE: u64 = 6;
}

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

/// A header for a display parcel of `method`. `ALLOW_NESTED` keeps an app's
/// event poll from tripping the kernel's per-channel cycle check while a
/// `Commit` call is in flight.
fn header(method: u32) -> Header {
    Header {
        version: VERSION,
        flags: flags::ALLOW_NESTED,
        interface_id: INTERFACE,
        method,
        txn_id: 0,
        reply_to: 0,
        deadline_ns: 0,
    }
}
