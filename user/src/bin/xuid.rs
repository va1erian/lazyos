//! `xuid`: the userspace session compositor (issue #113).
//!
//! `xuid` is the S4.4 compositor from `docs/platform-plan.md`: it binds the
//! kernel's display device grant, which hands it the screen (the mux stops
//! painting), the PS/2 input stream, and a mapped screen buffer. On top of that
//! it implements `os.lazy.display.v1` over Messenger:
//!
//! * `CreateSurface(width, height, title, events)` — the composer's event
//!   endpoint rides in the parcel's handle list; the reply carries the surface
//!   id;
//! * `AttachBuffer(surface, buffer)` — the pixel buffer rides in the parcel's
//!   buffer list and is mapped into the compositor;
//! * `Commit(surface, damage)` — the app says "these pixels are ready";
//! * `DestroySurface(surface)`.
//!
//! Input flows back to the focused surface's event endpoint as one-way
//! `PointerMove/Down/Up` and `KeyDown/Up` messages. Decorations (window body,
//! border, title bar and title) are painted by the compositor itself, and a
//! software cursor follows the pointer, since the kernel mux's cursor is gone
//! while it is bound.
//!
//! Issue #145 adds compositor-mediated drag & drop (methods 10–16): a source
//! hands over a clipboard token with `DragStart`, the compositor owns the
//! pointer while the button is held, and the surface under it receives
//! `DragEnter`/`DragOver`/`DragLeave` and finally `Drop` (with the token) or a
//! cancelled `DragEnded`. Escape cancels. See the "Drag & drop" section below
//! and `docs/architecture/display.md`.
//!
//! Boot it with `LAZYOS_XUID=1`; the kernel starts this program and `xdemo`.
//!
//! The compositing model is deliberately simple: one screen-sized RGBA buffer
//! and rectangle damage. `Commit` copies the app's damaged rectangle into the
//! screen buffer and presents exactly that rectangle, and pointer/focus changes
//! repaint the union of the old and new cursor or title rectangles. The app
//! buffer handoff is already zero-copy (the compositor reads the same frames
//! the app writes); fences and double buffering are the S8 follow-up that turns
//! `Commit` into a tear-free pipeline.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use libmessenger::{Decoder, Encoder, Kind, Parcel, VERSION};
use user::messenger::display::{self, Canvas, Color, Event, EventKind, Rect};
use user::messenger::{self, registry, Endpoint, Message};
use user::sys;

/// Title-bar height in pixels.
const TITLE_H: i32 = 22;
/// Window border thickness in pixels.
const BORDER: i32 = 2;
/// Where the first window's top-left sits.
const PAD: i32 = 48;

const BACKGROUND: Color = Color::rgb(18, 22, 36);
const WINDOW_BG: Color = Color::rgb(30, 36, 54);
const TITLE_BG: Color = Color::rgb(52, 60, 92);
const TITLE_BG_FOCUS: Color = Color::rgb(44, 112, 74);
const TITLE_TEXT: Color = Color::rgb(228, 232, 245);
const BORDER_COLOR: Color = Color::rgb(92, 106, 152);
const BORDER_COLOR_FOCUS: Color = Color::rgb(140, 220, 160);
const EMPTY_BG: Color = Color::rgb(16, 18, 28);
/// Drop-target frame and drag-label accent (issue #145).
const DRAG_ACCENT: Color = Color::rgb(245, 196, 84);
/// The drag label's chip background.
const DRAG_GHOST_BG: Color = Color::rgb(28, 24, 12);

/// The serial marker the evidence session greps for.
const UP_MARKER: &str = "XUID:UP:PASS\n";

/// One composited window.
struct Surface {
    /// Protocol id.
    id: u64,
    /// Window title from `CreateSurface`.
    title: String,
    /// Window top-left (content origin is `(x + BORDER, y + TITLE_H)`).
    x: i32,
    y: i32,
    /// Content size in pixels.
    w: i32,
    h: i32,
    /// Event endpoint handle in this task's table.
    events: u64,
    /// Task slot that created the surface; only it may start or cancel a drag
    /// for this surface (issue #145).
    owner: u64,
    /// App pixel buffer mapped into this task (`0` until attached).
    pixels: u64,
    /// Length of the mapped pixel buffer.
    bytes: u64,
}

impl Surface {
    /// The whole decorated window rectangle.
    fn window(&self) -> Rect {
        Rect::new(
            self.x,
            self.y,
            self.w + BORDER * 2,
            self.h + TITLE_H + BORDER,
        )
    }

    /// The title-bar rectangle.
    fn title_bar(&self) -> Rect {
        Rect::new(self.x, self.y, self.w + BORDER * 2, TITLE_H)
    }

    /// The content (app pixel) rectangle.
    fn content(&self) -> Rect {
        Rect::new(self.x + BORDER, self.y + TITLE_H, self.w, self.h)
    }
}

// ---------------------------------------------------------------------------
// Drag & drop (issue #145)
//
// The compositor owns the pointer while a drag is live: the source app hands
// over a clipboard token with `DragStart`, the surface under the pointer gets
// enter/leave/over notifications, and a release delivers `Drop` (with the
// token) or a cancelled `DragEnded`. All transitions live in this section;
// `handle_event`, `handle_request` and `repaint` only route into them, which
// keeps the window-management paths separate.
// ---------------------------------------------------------------------------

/// An active drag started by a client's `DragStart`.
struct Drag {
    /// Surface whose client started the drag.
    source: u64,
    /// Clipboard token delivered to the drop target.
    token: u64,
    /// MIME type of the token's payload; drawn as the drag label.
    mime: String,
    /// Surface currently under the pointer, if any (never the source).
    target: Option<u64>,
}

/// Find a surface by id.
fn surface_by_id(surfaces: &[Surface], id: u64) -> Option<&Surface> {
    surfaces.iter().find(|surface| surface.id == id)
}

/// The topmost surface whose content contains `point`, ignoring `source`.
fn drag_target_at(surfaces: &[Surface], source: u64, point: (i32, i32)) -> Option<u64> {
    surfaces
        .iter()
        .rev()
        .find(|surface| surface.id != source && contains(surface.content(), point))
        .map(|surface| surface.id)
}

/// The rectangle the drag ghost occupies around `point`.
fn ghost_rect(mime: &str, point: (i32, i32)) -> Rect {
    let label = (mime.len().min(24) as i32) * 6 + 8;
    Rect::new(point.0 + 6, point.1 + 6, 14 + label, 16)
}

/// Send one drag event (u64 fields plus an optional string) to a surface.
fn forward_drag(
    surfaces: &[Surface],
    scratch: &mut Vec<u8>,
    id: Option<u64>,
    method: u32,
    fields: &[(u16, u64)],
    text: Option<(u16, &str)>,
) {
    let Some(surface) = id.and_then(|id| surface_by_id(surfaces, id)) else {
        return;
    };
    let _ = display::send_event_fields(
        &Endpoint::from_raw(surface.events),
        scratch,
        method,
        fields,
        text,
    );
}

/// The `DragEnter`/`DragOver` payload fields for `point` over `id`.
fn drag_point_fields(surfaces: &[Surface], id: u64, point: (i32, i32)) -> [(u16, u64); 2] {
    let (x, y) = relative(surfaces, id, point);
    [(display::field::A, x as u64), (display::field::B, y as u64)]
}

/// Start a drag from `source`: adopt the token/mime, greet a surface already
/// under the pointer, and draw the ghost.
#[allow(clippy::too_many_arguments)]
fn drag_begin(
    drag: &mut Option<Drag>,
    surfaces: &[Surface],
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: Option<u64>,
    scratch: &mut Vec<u8>,
    source: u64,
    token: u64,
    mime: String,
) {
    let mut active = Drag {
        source,
        token,
        mime,
        target: None,
    };
    if let Some(id) = drag_target_at(surfaces, source, pointer) {
        let fields = drag_point_fields(surfaces, id, pointer);
        forward_drag(
            surfaces,
            scratch,
            Some(id),
            method::DRAG_ENTER,
            &fields,
            Some((display::field::MIME, &active.mime)),
        );
        active.target = Some(id);
    }
    let damage = cursor_rect(pointer).union(ghost_rect(&active.mime, pointer));
    *drag = Some(active);
    repaint(screen, surfaces, pointer, focused, damage, drag.as_ref());
}

/// Route a pointer move while a drag is live: the surface under the pointer
/// gets enter/over/leave; the source gets nothing until the drag ends.
/// Returns the damage the move needs.
fn drag_move(
    drag: &mut Drag,
    surfaces: &[Surface],
    pointer: (i32, i32),
    old: (i32, i32),
    scratch: &mut Vec<u8>,
) -> Rect {
    let mut damage = cursor_rect(old)
        .union(cursor_rect(pointer))
        .union(ghost_rect(&drag.mime, old))
        .union(ghost_rect(&drag.mime, pointer));
    let next = drag_target_at(surfaces, drag.source, pointer);
    if next != drag.target {
        if let Some(id) = drag.target {
            forward_drag(surfaces, scratch, Some(id), method::DRAG_LEAVE, &[], None);
            if let Some(surface) = surface_by_id(surfaces, id) {
                damage = damage.union(surface.window());
            }
        }
        drag.target = next;
        if let Some(id) = next {
            let fields = drag_point_fields(surfaces, id, pointer);
            forward_drag(
                surfaces,
                scratch,
                Some(id),
                method::DRAG_ENTER,
                &fields,
                Some((display::field::MIME, &drag.mime)),
            );
            if let Some(surface) = surface_by_id(surfaces, id) {
                damage = damage.union(surface.window());
            }
        }
    } else if let Some(id) = next {
        let fields = drag_point_fields(surfaces, id, pointer);
        forward_drag(
            surfaces,
            scratch,
            Some(id),
            method::DRAG_OVER,
            &fields,
            None,
        );
    }
    damage
}

/// Finish a drag at `pointer`: `Drop` the token on the surface under it, or
/// send `DragLeave` and a cancelled `DragEnded`.
fn drag_finish(
    drag: &mut Option<Drag>,
    surfaces: &[Surface],
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: Option<u64>,
    scratch: &mut Vec<u8>,
) {
    let Some(active) = drag.take() else {
        return;
    };
    let mut damage = cursor_rect(pointer).union(ghost_rect(&active.mime, pointer));
    match drag_target_at(surfaces, active.source, pointer) {
        Some(id) => {
            let fields = drag_point_fields(surfaces, id, pointer);
            forward_drag(
                surfaces,
                scratch,
                Some(id),
                method::DROP,
                &[fields[0], fields[1], (display::field::TOKEN, active.token)],
                Some((display::field::MIME, &active.mime)),
            );
            forward_drag(
                surfaces,
                scratch,
                Some(active.source),
                method::DRAG_ENDED,
                &[(display::field::A, 1)],
                None,
            );
            if let Some(surface) = surface_by_id(surfaces, id) {
                damage = damage.union(surface.window());
            }
        }
        None => {
            drag_leave_target(&active, surfaces, scratch, &mut damage);
            forward_drag(
                surfaces,
                scratch,
                Some(active.source),
                method::DRAG_ENDED,
                &[(display::field::A, 0)],
                None,
            );
        }
    }
    if let Some(surface) = surface_by_id(surfaces, active.source) {
        damage = damage.union(surface.window());
    }
    repaint(screen, surfaces, pointer, focused, damage, drag.as_ref());
}

/// Cancel a live drag (Escape, `DragCancel`, or the surface going away).
fn drag_cancel(
    drag: &mut Option<Drag>,
    surfaces: &[Surface],
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: Option<u64>,
    scratch: &mut Vec<u8>,
) {
    let Some(active) = drag.take() else {
        return;
    };
    let mut damage = cursor_rect(pointer).union(ghost_rect(&active.mime, pointer));
    drag_leave_target(&active, surfaces, scratch, &mut damage);
    forward_drag(
        surfaces,
        scratch,
        Some(active.source),
        method::DRAG_ENDED,
        &[(display::field::A, 0)],
        None,
    );
    if let Some(surface) = surface_by_id(surfaces, active.source) {
        damage = damage.union(surface.window());
    }
    repaint(screen, surfaces, pointer, focused, damage, drag.as_ref());
}

/// Notify a drag's current target that the drag left, growing `damage`.
fn drag_leave_target(
    active: &Drag,
    surfaces: &[Surface],
    scratch: &mut Vec<u8>,
    damage: &mut Rect,
) {
    if let Some(id) = active.target {
        forward_drag(surfaces, scratch, Some(id), method::DRAG_LEAVE, &[], None);
        if let Some(surface) = surface_by_id(surfaces, id) {
            *damage = damage.union(surface.window());
        }
    }
}

/// Paint the active drag over the composited frame: a frame around the target
/// surface and a payload ghost at the cursor.
fn draw_drag(
    screen: &mut Canvas,
    surfaces: &[Surface],
    drag: &Drag,
    pointer: (i32, i32),
    clip: Rect,
) {
    if let Some(surface) = drag.target.and_then(|id| surface_by_id(surfaces, id)) {
        let content = surface.content();
        screen.fill(
            Rect::new(content.x, content.y, content.w, 3),
            clip,
            DRAG_ACCENT,
        );
        screen.fill(
            Rect::new(content.x, content.y + content.h - 3, content.w, 3),
            clip,
            DRAG_ACCENT,
        );
        screen.fill(
            Rect::new(content.x, content.y, 3, content.h),
            clip,
            DRAG_ACCENT,
        );
        screen.fill(
            Rect::new(content.x + content.w - 3, content.y, 3, content.h),
            clip,
            DRAG_ACCENT,
        );
    }
    let ghost = ghost_rect(&drag.mime, pointer);
    screen.fill(Rect::new(ghost.x, ghost.y, 14, 14), clip, DRAG_ACCENT);
    screen.fill(
        Rect::new(ghost.x + 3, ghost.y + 3, 8, 8),
        clip,
        DRAG_GHOST_BG,
    );
    screen.fill(
        Rect::new(ghost.x + 14, ghost.y + 2, ghost.w - 14, 12),
        clip,
        DRAG_GHOST_BG,
    );
    screen.text(ghost.x + 18, ghost.y + 4, &drag.mime, DRAG_ACCENT, clip, 1);
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("xuid: session compositor starting (issue #113)\n");
    run()
}

/// Panic without unwinding: report on serial and let the kernel mux take over.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    sys::write_str("xuid: panic\n");
    let _ = info;
    sys::exit(1)
}

/// Bind the display, publish the protocol, and composite until the kernel
/// kills the task.
fn run() -> ! {
    // The display grant: on success the mux stops painting and input starts
    // arriving on the poll queue.
    let mut info = sys::DisplayInfo::default();
    if let Err(code) = sys::display_bind(&mut info) {
        fail("bind", code);
    }
    if info.width == 0 || info.height == 0 || info.va == 0 {
        fail("bind returned no screen buffer", info.size as i64);
    }

    // The published side is what clients resolve; requests arrive on `server`.
    let (published, server) = match messenger::create_pair() {
        Ok(pair) => pair,
        Err(error) => fail("create_pair", errno_code(error)),
    };
    if let Err(error) = registry::register(display::NAME, &published, &[display::INTERFACE], 0) {
        fail("register", errno_code(error));
    }
    sys::write_str("xuid: display bound, os.lazy.display.v1 published\n");

    let (screen_w, screen_h) = (info.width as i32, info.height as i32);
    let full = Rect::new(0, 0, screen_w, screen_h);
    // Safety: `va`/`size` come from the display bind and describe an RGBA8
    // screen buffer mapped in this task.
    let mut screen = unsafe { Canvas::new(info.va, screen_w, screen_h) };

    let mut surfaces: Vec<Surface> = Vec::new();
    let mut pointer = (screen_w / 2, screen_h / 2);
    let mut focused: Option<u64> = None;
    let mut next_id: u64 = 1;
    // Issue #145: the live drag, if any, and whether a pointer button is held
    // (`DragStart` requires it).
    let mut drag: Option<Drag> = None;
    let mut button_down = false;
    // One receive buffer and one event-encode buffer for the whole life of the
    // compositor: the user bump allocator never reclaims, so the loop reuses
    // both instead of allocating per message.
    let mut request_buf = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let mut event_scratch = Vec::with_capacity(64);

    // First frame: the previous mux pixels are still on screen, so paint the
    // desktop and present before announcing readiness.
    repaint(&mut screen, &surfaces, pointer, focused, full, None);
    sys::write_str(UP_MARKER);

    loop {
        // 1. Input: drain the kernel queue, then repaint only what changed.
        let mut events = [0u8; 16 * 32];
        loop {
            match sys::display_input_poll(&mut events) {
                Ok(0) => break,
                Ok(count) => {
                    for index in 0..count {
                        let Some(event) = decode_event(&events, index) else {
                            continue;
                        };
                        handle_event(
                            event,
                            &mut surfaces,
                            &mut screen,
                            &mut pointer,
                            &mut focused,
                            &mut event_scratch,
                            &mut drag,
                            &mut button_down,
                        );
                    }
                }
                Err(_) => break,
            }
        }

        // 2. Requests: serve one, then loop (the deadline bounds the nap when
        //    nothing is pending, keeping input latency at a couple of ticks).
        let deadline = Some(sys::clock() + 2);
        match server.recv_with(&mut request_buf, deadline) {
            Ok(message) => {
                if let Some(txn) = message.txn {
                    let reply = handle_request(
                        &message,
                        &mut surfaces,
                        &mut screen,
                        &mut next_id,
                        &mut focused,
                        pointer,
                        &mut drag,
                        &mut event_scratch,
                        button_down,
                    );
                    if let Some(reply) = reply {
                        let _ = server.reply(txn, &reply);
                    }
                }
            }
            Err(error) if is_timeout(error) => {}
            Err(messenger::Error::Errno(code)) if code == -messenger::errno::EPIPE => {
                // The client end went away; keep compositing for the others.
            }
            Err(_) => {}
        }
    }
}

/// Report a fatal startup failure on serial, then exit.
fn fail(what: &str, code: i64) -> ! {
    sys::write_str("xuid: fatal: ");
    sys::write_str(what);
    sys::write_str(": ");
    sys::write_str(&alloc::format!("{code}\n"));
    sys::exit(1)
}

/// The status code of a [`messenger::Error`], or `0` when it has none.
fn errno_code(error: messenger::Error) -> i64 {
    error.errno().unwrap_or(0)
}

/// Whether an error is the `recv` deadline firing.
fn is_timeout(error: messenger::Error) -> bool {
    matches!(error, messenger::Error::Errno(code) if code == -messenger::errno::ETIMEDOUT)
}

/// Decode the `index`-th 16-byte kernel event record.
fn decode_event(bytes: &[u8], index: usize) -> Option<Event> {
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

/// Route one input event: update focus/cursor, then forward it to the focused
/// surface's event endpoint. While a drag is live (issue #145) the compositor
/// keeps the pointer and routes it through the drag section above instead.
#[allow(clippy::too_many_arguments)]
fn handle_event(
    event: Event,
    surfaces: &mut Vec<Surface>,
    screen: &mut Canvas,
    pointer: &mut (i32, i32),
    focused: &mut Option<u64>,
    scratch: &mut Vec<u8>,
    drag: &mut Option<Drag>,
    button_down: &mut bool,
) {
    match event.kind {
        EventKind::PointerMove => {
            if drag.is_some() {
                let new = (event.a as i32, event.b as i32);
                let old = *pointer;
                *pointer = new;
                let damage = if let Some(active) = drag.as_mut() {
                    drag_move(active, surfaces, new, old, scratch)
                } else {
                    Rect::default()
                };
                repaint(screen, surfaces, *pointer, *focused, damage, drag.as_ref());
                return;
            }
            let new = (event.a as i32, event.b as i32);
            let old = *pointer;
            let damage = cursor_rect(old).union(cursor_rect(new));
            *pointer = new;
            forward(
                surfaces,
                scratch,
                *focused,
                method::POINTER_MOVE,
                event.a,
                event.b,
            );
            repaint(screen, surfaces, *pointer, *focused, damage, drag.as_ref());
        }
        EventKind::PointerDown => {
            *button_down = true;
            if drag.is_some() {
                // A second press while dragging is ignored; the drag ends on
                // the first release.
                return;
            }
            // Hit-test topmost first (the vector's tail is the top window).
            let hit = surfaces
                .iter()
                .rev()
                .find(|surface| contains(surface.window(), *pointer))
                .map(|surface| surface.id);
            if let Some(id) = hit {
                let old_title = *focused;
                *focused = Some(id);
                let mut damage = Rect::default();
                if let Some(surface) = surfaces.iter().find(|s| Some(s.id) == old_title) {
                    damage = damage.union(surface.title_bar());
                }
                if let Some(surface) = surfaces.iter().find(|s| Some(s.id) == hit) {
                    damage = damage.union(surface.title_bar());
                }
                repaint(screen, surfaces, *pointer, *focused, damage, drag.as_ref());
                let (x, y) = relative(surfaces, id, *pointer);
                forward(surfaces, scratch, Some(id), method::POINTER_DOWN, x, y);
            }
        }
        EventKind::PointerUp => {
            *button_down = false;
            if drag.is_some() {
                drag_finish(drag, surfaces, screen, *pointer, *focused, scratch);
                return;
            }
            let (x, y) = match *focused {
                Some(id) => relative(surfaces, id, *pointer),
                None => (0, 0),
            };
            forward(surfaces, scratch, *focused, method::POINTER_UP, x, y);
        }
        EventKind::KeyDown => {
            if drag.is_some() && event.a as u32 == display::key::ESCAPE {
                drag_cancel(drag, surfaces, screen, *pointer, *focused, scratch);
                return;
            }
            if event.a as u32 == display::key::TAB {
                cycle_focus(surfaces, focused);
                let damage = surfaces
                    .iter()
                    .map(|surface| surface.title_bar())
                    .fold(Rect::default(), Rect::union);
                repaint(screen, surfaces, *pointer, *focused, damage, drag.as_ref());
                return;
            }
            forward(
                surfaces,
                scratch,
                *focused,
                method::KEY_DOWN,
                event.a,
                event.b,
            );
        }
        EventKind::KeyUp => {
            forward(
                surfaces,
                scratch,
                *focused,
                method::KEY_UP,
                event.a,
                event.b,
            );
        }
    }
}

/// Whether `rect` contains the point `(x, y)`.
fn contains(rect: Rect, point: (i32, i32)) -> bool {
    point.0 >= rect.x && point.1 >= rect.y && point.0 < rect.x + rect.w && point.1 < rect.y + rect.h
}

/// The surface-relative pointer position inside a window's content.
fn relative(surfaces: &[Surface], id: u64, point: (i32, i32)) -> (i64, i64) {
    match surfaces.iter().find(|surface| surface.id == id) {
        Some(surface) => {
            let content = surface.content();
            ((point.0 - content.x) as i64, (point.1 - content.y) as i64)
        }
        None => (0, 0),
    }
}

/// Move focus to the next surface after the current one.
fn cycle_focus(surfaces: &[Surface], focused: &mut Option<u64>) {
    if surfaces.is_empty() {
        *focused = None;
        return;
    }
    let current = focused
        .and_then(|id| surfaces.iter().position(|surface| surface.id == id))
        .unwrap_or(0);
    *focused = Some(surfaces[(current + 1) % surfaces.len()].id);
}

/// Send one event to a surface's endpoint, ignoring a closed peer.
fn forward(
    surfaces: &[Surface],
    scratch: &mut Vec<u8>,
    id: Option<u64>,
    method: u32,
    a: i64,
    b: i64,
) {
    let Some(surface) = id.and_then(|id| surfaces.iter().find(|surface| surface.id == id)) else {
        return;
    };
    let _ = display::send_event(
        &Endpoint::from_raw(surface.events),
        scratch,
        method,
        a as u64,
        b as u64,
    );
}

/// The 10x10 sprite rectangle the cursor occupies at `point`.
fn cursor_rect(point: (i32, i32)) -> Rect {
    Rect::new(point.0 - 1, point.1 - 1, 11, 11)
}

/// Compose `damage` from the background, every intersecting window, the active
/// drag (if any), and the cursor, then present exactly that rectangle.
fn repaint(
    screen: &mut Canvas,
    surfaces: &[Surface],
    pointer: (i32, i32),
    focused: Option<u64>,
    damage: Rect,
    drag: Option<&Drag>,
) {
    if damage.is_empty() {
        return;
    }
    screen.fill(damage, damage, BACKGROUND);
    for surface in surfaces {
        draw_surface(screen, surface, focused == Some(surface.id), damage);
    }
    if let Some(drag) = drag {
        draw_drag(screen, surfaces, drag, pointer, damage);
    }
    screen.cursor(pointer.0, pointer.1, damage);
    let _ = sys::display_present(damage.x, damage.y, damage.w, damage.h);
}

/// Draw one decorated window, clipped to `clip`.
fn draw_surface(screen: &mut Canvas, surface: &Surface, focused: bool, clip: Rect) {
    let window = surface.window();
    if window.intersect(clip).is_empty() {
        return;
    }
    let border = if focused {
        BORDER_COLOR_FOCUS
    } else {
        BORDER_COLOR
    };
    // Body, then a 1px frame and the title separator.
    screen.fill(window, clip, WINDOW_BG);
    screen.fill(Rect::new(window.x, window.y, window.w, 1), clip, border);
    screen.fill(
        Rect::new(window.x, window.y + window.h - 1, window.w, 1),
        clip,
        border,
    );
    screen.fill(Rect::new(window.x, window.y, 1, window.h), clip, border);
    screen.fill(
        Rect::new(window.x + window.w - 1, window.y, 1, window.h),
        clip,
        border,
    );
    // Title bar.
    screen.fill(
        surface.title_bar(),
        clip,
        if focused { TITLE_BG_FOCUS } else { TITLE_BG },
    );
    screen.fill(
        Rect::new(window.x, surface.y + TITLE_H, window.w, 1),
        clip,
        border,
    );
    screen.text(
        surface.x + 8,
        surface.y + 8,
        &surface.title,
        TITLE_TEXT,
        clip,
        1,
    );

    // The app's pixels, or an explicit placeholder before AttachBuffer.
    let content = surface.content();
    if surface.pixels != 0 && surface.bytes >= (surface.w * surface.h * 4) as u64 {
        // Safety: the mapping was installed by `display_map_buffer` for this
        // buffer and the surface's geometry describes it.
        let pixels = unsafe {
            core::slice::from_raw_parts(surface.pixels as *const u8, surface.bytes as usize)
        };
        screen.blit(pixels, surface.w, surface.h, content, clip);
    } else {
        screen.fill(content, clip, EMPTY_BG);
        screen.text(
            content.x + 10,
            content.y + 10,
            "waiting for buffer",
            TITLE_TEXT,
            clip,
            1,
        );
    }
}

/// Handle one display request; returns the reply parcel for a synchronous call.
#[allow(clippy::too_many_arguments)]
fn handle_request(
    message: &Message,
    surfaces: &mut Vec<Surface>,
    screen: &mut Canvas,
    next_id: &mut u64,
    focused: &mut Option<u64>,
    pointer: (i32, i32),
    drag: &mut Option<Drag>,
    scratch: &mut Vec<u8>,
    button_down: bool,
) -> Option<Parcel> {
    if message.interface_id() != display::INTERFACE {
        return Some(empty_reply(message.method()));
    }
    match message.method() {
        method::CREATE_SURFACE => {
            let width = u64_field(&message.parcel, display::field::WIDTH).unwrap_or(0);
            let height = u64_field(&message.parcel, display::field::HEIGHT).unwrap_or(0);
            let title = string_field(&message.parcel, display::field::TITLE)
                .unwrap_or_else(|| String::from("app"));
            if width == 0 || height == 0 || message.handles == 0 {
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            }
            let id = *next_id;
            *next_id += 1;
            // Lay windows out left to right at the top, cascading down when
            // the row is full, so every surface is visible at once.
            let count = surfaces.len() as i32;
            let x = match surfaces.last() {
                Some(previous) => previous.x + previous.window().w + 16,
                None => PAD,
            };
            let x = x.min((screen.width() - width as i32 - 32).max(0));
            let y = PAD + (count / 3) * (height as i32 + TITLE_H + 32);
            let y = y.min((screen.height() - height as i32 - 64).max(0));
            surfaces.push(Surface {
                id,
                title,
                x,
                y,
                w: width as i32,
                h: height as i32,
                events: message.first_handle,
                owner: message.sender,
                pixels: 0,
                bytes: 0,
            });
            if focused.is_none() {
                *focused = Some(id);
            }
            let damage = surfaces
                .last()
                .map(|surface| surface.window())
                .unwrap_or_default();
            repaint(screen, surfaces, pointer, *focused, damage, drag.as_ref());
            let mut body = Encoder::new();
            let _ = body.u64(display::field::SURFACE, id);
            Some(reply_parcel(message.method(), body))
        }
        method::ATTACH_BUFFER => {
            let id = u64_field(&message.parcel, display::field::SURFACE).unwrap_or(0);
            let Some(surface) = surfaces.iter_mut().find(|surface| surface.id == id) else {
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            };
            // The descriptor's length is the sender's claim about how many
            // bytes the surface needs; never trust it to cover the geometry
            // the compositor paints.
            let expected = (surface.w * surface.h * 4) as u64;
            let claimed = message
                .parcel
                .buffers
                .first()
                .map(|buffer| buffer.len)
                .unwrap_or(0);
            if message.buffers == 0 || claimed < expected {
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            }
            match sys::display_map_buffer(message.first_buffer) {
                Ok(va) => {
                    surface.pixels = va;
                    surface.bytes = expected;
                    let damage = surface.window();
                    repaint(screen, surfaces, pointer, *focused, damage, drag.as_ref());
                    Some(empty_reply(message.method()))
                }
                Err(code) => Some(error_reply(message.method(), -code)),
            }
        }
        method::COMMIT => {
            let id = u64_field(&message.parcel, display::field::SURFACE).unwrap_or(0);
            if let Some(surface) = surfaces.iter().find(|surface| surface.id == id) {
                let content = surface.content();
                let damage = Rect::new(
                    content.x + u64_field(&message.parcel, display::field::X).unwrap_or(0) as i32,
                    content.y + u64_field(&message.parcel, display::field::Y).unwrap_or(0) as i32,
                    u64_field(&message.parcel, display::field::W).unwrap_or(0) as i32,
                    u64_field(&message.parcel, display::field::H).unwrap_or(0) as i32,
                )
                .intersect(content);
                repaint(screen, surfaces, pointer, *focused, damage, drag.as_ref());
            }
            Some(empty_reply(message.method()))
        }
        method::DESTROY_SURFACE => {
            let id = u64_field(&message.parcel, display::field::SURFACE).unwrap_or(0);
            // A drag whose source or hovered target goes away ends now.
            let stranding = drag
                .as_ref()
                .is_some_and(|active| active.source == id || active.target == Some(id));
            if stranding {
                drag_cancel(drag, surfaces, screen, pointer, *focused, scratch);
            }
            let damage = surfaces
                .iter()
                .find(|surface| surface.id == id)
                .map(|surface| surface.window());
            surfaces.retain(|surface| surface.id != id);
            if *focused == Some(id) {
                *focused = surfaces.first().map(|surface| surface.id);
            }
            if let Some(damage) = damage {
                repaint(screen, surfaces, pointer, *focused, damage, drag.as_ref());
            }
            Some(empty_reply(message.method()))
        }
        method::DRAG_START => {
            let id = u64_field(&message.parcel, display::field::SURFACE).unwrap_or(0);
            let token = u64_field(&message.parcel, display::field::TOKEN).unwrap_or(0);
            let mime = string_field(&message.parcel, display::field::MIME).unwrap_or_default();
            if drag.is_some() {
                return Some(error_reply(message.method(), messenger::errno::EBUSY));
            }
            let Some(surface) = surface_by_id(surfaces, id) else {
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            };
            // Only the surface's own client may drag from it, and only with a
            // pointer button held: the gesture is what makes it a drag.
            if surface.owner != message.sender {
                return Some(error_reply(message.method(), messenger::errno::EACCES));
            }
            if token == 0 || mime.is_empty() || mime.len() > display::MAX_MIME || !button_down {
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            }
            drag_begin(
                drag, surfaces, screen, pointer, *focused, scratch, id, token, mime,
            );
            Some(empty_reply(message.method()))
        }
        method::DRAG_CANCEL => {
            let id = u64_field(&message.parcel, display::field::SURFACE).unwrap_or(0);
            let owns = drag.as_ref().is_some_and(|active| active.source == id)
                && surface_by_id(surfaces, id).is_some_and(|s| s.owner == message.sender);
            if owns {
                drag_cancel(drag, surfaces, screen, pointer, *focused, scratch);
            }
            Some(empty_reply(message.method()))
        }
        _ => Some(error_reply(message.method(), messenger::errno::EINVAL)),
    }
}

/// The protocol method ids. (The client mirror in `user::messenger::display`
/// has the same values; a compositor binary is not generic over them.)
mod method {
    pub const CREATE_SURFACE: u32 = 1;
    pub const ATTACH_BUFFER: u32 = 2;
    pub const COMMIT: u32 = 3;
    pub const DESTROY_SURFACE: u32 = 4;
    pub const POINTER_MOVE: u32 = 5;
    pub const POINTER_DOWN: u32 = 6;
    pub const POINTER_UP: u32 = 7;
    pub const KEY_DOWN: u32 = 8;
    pub const KEY_UP: u32 = 9;
    pub const DRAG_START: u32 = 10;
    pub const DRAG_CANCEL: u32 = 11;
    pub const DRAG_ENTER: u32 = 12;
    pub const DRAG_OVER: u32 = 13;
    pub const DRAG_LEAVE: u32 = 14;
    pub const DROP: u32 = 15;
    pub const DRAG_ENDED: u32 = 16;
}

/// Find the first `u64` field with `id`.
fn u64_field(parcel: &Parcel, id: u16) -> Option<u64> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::U64 && field.id == id {
            return field.as_u64().ok();
        }
    }
    None
}

/// Find the first string field with `id`.
fn string_field(parcel: &Parcel, id: u16) -> Option<String> {
    let mut decoder = Decoder::new(&parcel.body);
    while let Ok(Some(field)) = decoder.next() {
        if field.kind == Kind::String && field.id == id {
            return field.as_str().ok().map(String::from);
        }
    }
    None
}

/// An empty reply carrying only the header.
fn empty_reply(method: u32) -> Parcel {
    reply_parcel(method, Encoder::new())
}

/// An error reply: an `Error` TLV with a positive code, as the daemon
/// convention in this codebase uses.
fn error_reply(method: u32, code: i64) -> Parcel {
    let mut body = Encoder::new();
    let _ = body.error(display::field::ERROR, code as u32, "display request failed");
    reply_parcel(method, body)
}

/// Build a reply parcel for `method`.
fn reply_parcel(method: u32, body: Encoder) -> Parcel {
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
