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
//! Window management (issue #143) is built on the same model:
//!
//! * the `surfaces` vector *is* the z-order — its tail paints last, and a click
//!   raises that surface to the tail and focuses it;
//! * the title bar is a drag handle: a left press on it grabs the window and
//!   subsequent pointer movement moves the origin, clamped to the screen (and
//!   to the space above the taskbar);
//! * the title bar carries close (`X`, asks the client to exit via a one-way
//!   `WindowClose` event) and minimize (`-`, hides the surface) buttons;
//! * a bottom taskbar strip lists every live surface by title in creation
//!   order; clicking an entry focuses/raises it, and restores it when minimized.
//!   The focused entry is highlighted;
//! * `Tab` cycles focus across visible surfaces only, skipping minimized ones.
//!
//! Issue #145 adds compositor-mediated drag & drop (methods 11–17): a source
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
//! screen buffer and presents exactly that rectangle; window-management events
//! are layout changes and repaint the full screen (a title-bar drag repaints
//! the union of the old and new window rectangles, which redraws every surface
//! in z-order inside that damage). The app buffer handoff is already zero-copy
//! (the compositor reads the same frames the app writes); fences and double
//! buffering are the S8 follow-up that turns `Commit` into a tear-free
//! pipeline.

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
/// Taskbar height in pixels.
const TASKBAR_H: i32 = 28;
/// Taskbar entry height in pixels.
const ENTRY_H: i32 = 20;
/// Horizontal gap between taskbar entries.
const ENTRY_GAP: i32 = 4;
/// Taskbar margin before the first and after the last entry.
const ENTRY_MARGIN: i32 = 6;
/// Horizontal padding inside a taskbar entry, per side.
const ENTRY_PAD: i32 = 8;
/// Smallest taskbar entry width.
const ENTRY_MIN_W: i32 = 48;
/// Title-bar button size in pixels.
const BUTTON: i32 = 16;
/// Gap between the two title-bar buttons.
const BUTTON_GAP: i32 = 2;
/// Distance from the button group to the window's right edge.
const BUTTON_MARGIN: i32 = 3;

const BACKGROUND: Color = Color::rgb(18, 22, 36);
const WINDOW_BG: Color = Color::rgb(30, 36, 54);
const TITLE_BG: Color = Color::rgb(52, 60, 92);
const TITLE_BG_FOCUS: Color = Color::rgb(44, 112, 74);
const TITLE_TEXT: Color = Color::rgb(228, 232, 245);
const BORDER_COLOR: Color = Color::rgb(92, 106, 152);
const BORDER_COLOR_FOCUS: Color = Color::rgb(140, 220, 160);
const EMPTY_BG: Color = Color::rgb(16, 18, 28);
const TASKBAR_BG: Color = Color::rgb(24, 28, 44);
const TASKBAR_ENTRY: Color = Color::rgb(52, 60, 92);
const TASKBAR_ENTRY_MIN: Color = Color::rgb(38, 44, 66);
const TASKBAR_ENTRY_FOCUS: Color = Color::rgb(44, 112, 74);
const CLOSE_BG: Color = Color::rgb(198, 76, 76);
const MINIMIZE_BG: Color = Color::rgb(208, 168, 88);
const BUTTON_TEXT: Color = Color::rgb(24, 24, 32);
/// Drop-target frame and drag-label accent (issue #145).
const DRAG_ACCENT: Color = Color::rgb(245, 196, 84);
/// The drag label's chip background.
const DRAG_GHOST_BG: Color = Color::rgb(28, 24, 12);

/// The serial marker the evidence session greps for.
const UP_MARKER: &str = "XUID:UP:PASS\n";
/// The marker that says the window-management features came up.
const WM_MARKER: &str = "XUID:WM:PASS\n";

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
    /// Hidden by the minimize button; restorable from the taskbar.
    minimized: bool,
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

    /// The close button, inset in the title bar's right end.
    fn close_button(&self) -> Rect {
        Rect::new(
            self.x + self.w + BORDER * 2 - BUTTON_MARGIN - BUTTON,
            self.y + (TITLE_H - BUTTON) / 2,
            BUTTON,
            BUTTON,
        )
    }

    /// The minimize button, just left of the close button.
    fn minimize_button(&self) -> Rect {
        let close = self.close_button();
        Rect::new(close.x - BUTTON - BUTTON_GAP, close.y, BUTTON, BUTTON)
    }
}

/// An in-progress title-bar drag.
#[derive(Clone, Copy)]
struct Drag {
    /// The surface being moved.
    id: u64,
    /// Pointer offset from the window origin at grab time.
    grab_x: i32,
    grab_y: i32,
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

/// An active drag & drop session started by a client's `DragStart`.
struct DragSession {
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

/// The topmost visible surface whose content contains `point`, ignoring
/// `source`.
fn drag_target_at(surfaces: &[Surface], source: u64, point: (i32, i32)) -> Option<u64> {
    surfaces
        .iter()
        .rev()
        .find(|surface| {
            !surface.minimized && surface.id != source && contains(surface.content(), point)
        })
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
    drag: &mut Option<DragSession>,
    surfaces: &[Surface],
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: Option<u64>,
    scratch: &mut Vec<u8>,
    source: u64,
    token: u64,
    mime: String,
) {
    let mut active = DragSession {
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
    drag: &mut DragSession,
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
    drag: &mut Option<DragSession>,
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
    drag: &mut Option<DragSession>,
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
    active: &DragSession,
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

/// Paint the active drag & drop session over the composited frame: a frame
/// around the target surface and a payload ghost at the cursor.
fn draw_drag(
    screen: &mut Canvas,
    surfaces: &[Surface],
    session: &DragSession,
    pointer: (i32, i32),
    clip: Rect,
) {
    if let Some(surface) = session.target.and_then(|id| surface_by_id(surfaces, id)) {
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
    let ghost = ghost_rect(&session.mime, pointer);
    screen.fill(Rect::new(ghost.x, ghost.y, 14, 14), clip, DRAG_ACCENT);
    screen.fill(
        Rect::new(ghost.x + 3, ghost.y + 3, 8, 8),
        clip,
        DRAG_GHOST_BG,
    );
    let label = Rect::new(ghost.x + 14, ghost.y + 2, ghost.w - 14, 12);
    screen.fill(label, clip, DRAG_GHOST_BG);
    // `ghost_rect` reserves room for 24 characters but a MIME string may be
    // up to `display::MAX_MIME`; clip the text to the label so glyphs past it
    // (outside the drag's damage) cannot leave trails as the pointer moves.
    screen.text(
        ghost.x + 18,
        ghost.y + 4,
        &session.mime,
        DRAG_ACCENT,
        clip.intersect(label),
        1,
    );
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
    // The window-manager title-bar drag (issue #143).
    let mut drag: Option<Drag> = None;
    // Issue #145: the live drag & drop session, if any, and whether a pointer
    // button is held (`DragStart` requires it).
    let mut drag_session: Option<DragSession> = None;
    let mut button_down = false;
    // Buttons whose press the compositor consumed (taskbar, window buttons,
    // title bar, desktop): their release is swallowed too, so no surface sees
    // an unmatched `POINTER_UP` even if focus moved in between.
    let mut consumed: u32 = 0;
    let mut next_id: u64 = 1;
    // One receive buffer and one event-encode buffer for the whole life of the
    // compositor: the user bump allocator never reclaims, so the loop reuses
    // both instead of allocating per message.
    let mut request_buf = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let mut event_scratch = Vec::with_capacity(64);

    // First frame: the previous mux pixels are still on screen, so paint the
    // desktop and present before announcing readiness.
    repaint(&mut screen, &surfaces, pointer, focused, full, None);
    sys::write_str(UP_MARKER);
    sys::write_str(WM_MARKER);

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
                            &mut drag,
                            &mut drag_session,
                            &mut event_scratch,
                            &mut button_down,
                            &mut consumed,
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
                        &mut drag,
                        pointer,
                        &mut drag_session,
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

/// Route one input event: window-management actions first (taskbar, title-bar
/// buttons, drag, raise), then focus/cursor updates, then forward the event to
/// the focused surface's event endpoint. While a drag & drop session is live
/// (issue #145) the compositor owns the pointer and routes it through the drag
/// section above instead.
#[allow(clippy::too_many_arguments)]
fn handle_event(
    event: Event,
    surfaces: &mut Vec<Surface>,
    screen: &mut Canvas,
    pointer: &mut (i32, i32),
    focused: &mut Option<u64>,
    drag: &mut Option<Drag>,
    drag_session: &mut Option<DragSession>,
    scratch: &mut Vec<u8>,
    button_down: &mut bool,
    consumed: &mut u32,
) {
    let (screen_w, screen_h) = (screen.width(), screen.height());
    // `consumed` bit for the button in a press/release event.
    let button_bit = 1u32 << (event.a as u32 & 31);
    let full = Rect::new(0, 0, screen_w, screen_h);
    match event.kind {
        EventKind::PointerMove => {
            let new = (event.a as i32, event.b as i32);
            let old = *pointer;
            // A drag & drop session owns the pointer (issue #145): the surface
            // under it gets enter/over/leave, and the source hears nothing
            // until the session ends.
            if let Some(active) = drag_session.as_mut() {
                *pointer = new;
                let damage = drag_move(active, surfaces, new, old, scratch);
                repaint(
                    screen,
                    surfaces,
                    *pointer,
                    *focused,
                    damage,
                    drag_session.as_ref(),
                );
                return;
            }
            let mut damage = cursor_rect(old).union(cursor_rect(new));
            *pointer = new;
            if let Some(active) = *drag {
                // A title-bar drag: place the window so the grabbed point
                // stays under the pointer (exact even if events were
                // coalesced), clamped to the screen and the space above the
                // taskbar.
                if let Some(index) = surfaces
                    .iter()
                    .position(|surface| surface.id == active.id && !surface.minimized)
                {
                    let window = surfaces[index].window();
                    let target_x = new.0 - active.grab_x;
                    let target_y = new.1 - active.grab_y;
                    let surface = &mut surfaces[index];
                    surface.x = target_x.clamp(0, (screen_w - window.w).max(0));
                    surface.y = target_y.clamp(0, (screen_h - TASKBAR_H - window.h).max(0));
                    damage = damage.union(window).union(surface.window());
                }
                // The matching press was consumed by the title bar, so the
                // moves stay in the compositor: the app never saw the grab.
                repaint(
                    screen,
                    surfaces,
                    *pointer,
                    *focused,
                    damage,
                    drag_session.as_ref(),
                );
                return;
            }
            forward(
                surfaces,
                scratch,
                *focused,
                method::POINTER_MOVE,
                event.a,
                event.b,
            );
            repaint(
                screen,
                surfaces,
                *pointer,
                *focused,
                damage,
                drag_session.as_ref(),
            );
        }
        EventKind::PointerDown => {
            *button_down = true;
            if drag_session.is_some() {
                // A second press while a drag & drop session is live is
                // ignored; the session ends on the first release.
                return;
            }
            let point = *pointer;
            // The taskbar paints above every window, so it hit-tests first.
            if let Some(id) = taskbar_hit(surfaces, screen_w, screen_h, point) {
                *consumed |= button_bit;
                restore(surfaces, focused, id);
                repaint(
                    screen,
                    surfaces,
                    *pointer,
                    *focused,
                    full,
                    drag_session.as_ref(),
                );
                return;
            }
            // A press outside every window is a desktop click: ignore it.
            let Some((id, origin, close, minimize, title)) = surfaces
                .iter()
                .rev()
                .find(|surface| !surface.minimized && contains(surface.window(), point))
                .map(|surface| {
                    (
                        surface.id,
                        (surface.x, surface.y),
                        surface.close_button(),
                        surface.minimize_button(),
                        surface.title_bar(),
                    )
                })
            else {
                *consumed |= button_bit;
                return;
            };
            raise(surfaces, id);
            *focused = Some(id);
            let left = event.a as u32 == display::button::LEFT;
            if left && contains(close, point) {
                *consumed |= button_bit;
                close_surface(
                    surfaces,
                    screen,
                    *pointer,
                    focused,
                    scratch,
                    drag_session.as_ref(),
                    id,
                );
                return;
            }
            if left && contains(minimize, point) {
                *consumed |= button_bit;
                minimize_surface(
                    surfaces,
                    screen,
                    *pointer,
                    focused,
                    drag_session.as_ref(),
                    id,
                );
                return;
            }
            if contains(title, point) {
                *consumed |= button_bit;
                if left {
                    *drag = Some(Drag {
                        id,
                        grab_x: point.0 - origin.0,
                        grab_y: point.1 - origin.1,
                    });
                }
                // Title-bar presses (and a right-click that cannot drag) are
                // the WM's; only the focus/raise repaint is needed.
                repaint(
                    screen,
                    surfaces,
                    *pointer,
                    *focused,
                    full,
                    drag_session.as_ref(),
                );
                return;
            }
            // Content: focus, raise, and forward the press surface-relative.
            repaint(
                screen,
                surfaces,
                *pointer,
                *focused,
                full,
                drag_session.as_ref(),
            );
            let (x, y) = relative(surfaces, id, point);
            forward(surfaces, scratch, Some(id), method::POINTER_DOWN, x, y);
        }
        EventKind::PointerUp => {
            *button_down = false;
            if drag_session.is_some() {
                *consumed &= !button_bit;
                drag_finish(drag_session, surfaces, screen, *pointer, *focused, scratch);
                return;
            }
            if *consumed & button_bit != 0 {
                // The matching press was the compositor's (taskbar, window
                // button, title bar or desktop): swallow its release, and end
                // a title-bar drag it started.
                *consumed &= !button_bit;
                if event.a as u32 == display::button::LEFT {
                    *drag = None;
                }
                return;
            }
            let (x, y) = match *focused {
                Some(id) => relative(surfaces, id, *pointer),
                None => (0, 0),
            };
            forward(surfaces, scratch, *focused, method::POINTER_UP, x, y);
        }
        EventKind::KeyDown => {
            if drag_session.is_some() && event.a as u32 == display::key::ESCAPE {
                drag_cancel(drag_session, surfaces, screen, *pointer, *focused, scratch);
                return;
            }
            if event.a as u32 == display::key::TAB {
                cycle_focus(surfaces, focused);
                repaint(
                    screen,
                    surfaces,
                    *pointer,
                    *focused,
                    full,
                    drag_session.as_ref(),
                );
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

/// Move a surface to the tail of `surfaces`, i.e. the top of the paint order.
fn raise(surfaces: &mut Vec<Surface>, id: u64) {
    if let Some(index) = surfaces.iter().position(|surface| surface.id == id) {
        if index + 1 != surfaces.len() {
            let surface = surfaces.remove(index);
            surfaces.push(surface);
        }
    }
}

/// The topmost visible surface's id.
fn topmost_visible(surfaces: &[Surface]) -> Option<u64> {
    surfaces
        .iter()
        .rev()
        .find(|surface| !surface.minimized)
        .map(|surface| surface.id)
}

/// Focus a taskbar entry: restore it if minimized, raise it, and focus it.
fn restore(surfaces: &mut Vec<Surface>, focused: &mut Option<u64>, id: u64) {
    if let Some(surface) = surfaces.iter_mut().find(|surface| surface.id == id) {
        surface.minimized = false;
    }
    raise(surfaces, id);
    *focused = Some(id);
}

/// Minimize a surface, moving focus to the next visible surface.
fn minimize_surface(
    surfaces: &mut Vec<Surface>,
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: &mut Option<u64>,
    drag_session: Option<&DragSession>,
    id: u64,
) {
    if let Some(surface) = surfaces.iter_mut().find(|surface| surface.id == id) {
        surface.minimized = true;
    }
    if *focused == Some(id) {
        *focused = topmost_visible(surfaces);
    }
    let full = Rect::new(0, 0, screen.width(), screen.height());
    repaint(screen, surfaces, pointer, *focused, full, drag_session);
}

/// Close a surface: tell the client through a one-way `WindowClose` event and
/// drop it; the full-screen repaint lets the windows below show through.
fn close_surface(
    surfaces: &mut Vec<Surface>,
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: &mut Option<u64>,
    scratch: &mut Vec<u8>,
    drag_session: Option<&DragSession>,
    id: u64,
) {
    if let Some(surface) = surfaces.iter().find(|surface| surface.id == id) {
        let _ = display::send_event(
            &Endpoint::from_raw(surface.events),
            scratch,
            method::WINDOW_CLOSE,
            0,
            0,
        );
    }
    remove_surface(surfaces, id);
    if *focused == Some(id) {
        *focused = topmost_visible(surfaces);
    }
    let full = Rect::new(0, 0, screen.width(), screen.height());
    repaint(screen, surfaces, pointer, *focused, full, drag_session);
}

/// Drop surface `id` and close its transferred event endpoint, so repeated
/// create/destroy cycles cannot exhaust this task's handle table. Events
/// already queued (e.g. `WindowClose`) stay deliverable after the close.
fn remove_surface(surfaces: &mut Vec<Surface>, id: u64) {
    if let Some(index) = surfaces.iter().position(|surface| surface.id == id) {
        let surface = surfaces.remove(index);
        if surface.events != 0 {
            let _ = Endpoint::from_raw(surface.events).close();
        }
    }
}

/// Move focus to the next visible surface, wrapping around and skipping
/// minimized ones; the new focus is raised so its title bar is not covered.
fn cycle_focus(surfaces: &mut Vec<Surface>, focused: &mut Option<u64>) {
    if surfaces.iter().all(|surface| surface.minimized) {
        *focused = None;
        return;
    }
    let current_id = *focused;
    if let Some(current) = current_id.and_then(|id| surfaces.iter().position(|s| s.id == id)) {
        for step in 1..=surfaces.len() {
            let index = (current + step) % surfaces.len();
            if !surfaces[index].minimized && Some(surfaces[index].id) != current_id {
                let id = surfaces[index].id;
                raise(surfaces, id);
                *focused = Some(id);
                return;
            }
        }
    }
    // No other visible surface: focus (and raise) the first visible one.
    if let Some(id) = surfaces
        .iter()
        .find(|surface| !surface.minimized)
        .map(|surface| surface.id)
    {
        raise(surfaces, id);
        *focused = Some(id);
    }
}

/// The width of a surface's taskbar entry: title width plus padding.
fn entry_width(surface: &Surface) -> i32 {
    (surface.title.chars().count() as i32 * display::font::ADVANCE + ENTRY_PAD * 2).max(ENTRY_MIN_W)
}

/// Visit every taskbar entry in stable creation (id) order, left to right.
fn for_each_entry(
    surfaces: &[Surface],
    screen_w: i32,
    screen_h: i32,
    mut visit: impl FnMut(&Surface, Rect),
) {
    let mut x = ENTRY_MARGIN;
    let mut last_id = 0u64;
    let y = screen_h - TASKBAR_H + (TASKBAR_H - ENTRY_H) / 2;
    loop {
        let Some(surface) = surfaces
            .iter()
            .filter(|surface| surface.id > last_id)
            .min_by_key(|surface| surface.id)
        else {
            break;
        };
        last_id = surface.id;
        let width = entry_width(surface);
        if x + width > screen_w - ENTRY_MARGIN {
            break;
        }
        visit(surface, Rect::new(x, y, width, ENTRY_H));
        x += width + ENTRY_GAP;
    }
}

/// The taskbar entry under `point`, if any.
fn taskbar_hit(
    surfaces: &[Surface],
    screen_w: i32,
    screen_h: i32,
    point: (i32, i32),
) -> Option<u64> {
    let mut hit = None;
    for_each_entry(surfaces, screen_w, screen_h, |surface, rect| {
        if hit.is_none() && contains(rect, point) {
            hit = Some(surface.id);
        }
    });
    hit
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

/// Compose `damage` from the background, every visible window in z-order, the
/// taskbar, the active drag & drop session (if any), and the cursor, then
/// present exactly that rectangle.
fn repaint(
    screen: &mut Canvas,
    surfaces: &[Surface],
    pointer: (i32, i32),
    focused: Option<u64>,
    damage: Rect,
    drag_session: Option<&DragSession>,
) {
    if damage.is_empty() {
        return;
    }
    screen.fill(damage, damage, BACKGROUND);
    for surface in surfaces.iter().filter(|surface| !surface.minimized) {
        draw_surface(screen, surface, focused == Some(surface.id), damage);
    }
    draw_taskbar(screen, surfaces, focused, damage);
    if let Some(session) = drag_session {
        draw_drag(screen, surfaces, session, pointer, damage);
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
    // The title stops before the button group on the right.
    let reserved = BUTTON * 2 + BUTTON_GAP + BUTTON_MARGIN + 6;
    let title_clip = Rect::new(
        window.x + 2,
        surface.y,
        (window.w - 2 - reserved).max(0),
        TITLE_H,
    )
    .intersect(clip);
    screen.text(
        surface.x + 8,
        surface.y + 8,
        &surface.title,
        TITLE_TEXT,
        title_clip,
        1,
    );
    // Close and minimize buttons, painted over the title bar.
    for (rect, background, glyph) in [
        (surface.close_button(), CLOSE_BG, "X"),
        (surface.minimize_button(), MINIMIZE_BG, "-"),
    ] {
        screen.fill(rect, clip, background);
        screen.text(rect.x + 5, rect.y + 4, glyph, BUTTON_TEXT, clip, 1);
    }

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

/// Draw the bottom taskbar: one entry per live surface in creation order, with
/// the focused entry highlighted and minimized ones dimmed.
fn draw_taskbar(screen: &mut Canvas, surfaces: &[Surface], focused: Option<u64>, clip: Rect) {
    let (screen_w, screen_h) = (screen.width(), screen.height());
    let bar = Rect::new(0, screen_h - TASKBAR_H, screen_w, TASKBAR_H);
    if bar.intersect(clip).is_empty() {
        return;
    }
    screen.fill(bar, clip, TASKBAR_BG);
    screen.fill(Rect::new(bar.x, bar.y, bar.w, 1), clip, BORDER_COLOR);
    for_each_entry(surfaces, screen_w, screen_h, |surface, rect| {
        let background = if focused == Some(surface.id) {
            TASKBAR_ENTRY_FOCUS
        } else if surface.minimized {
            TASKBAR_ENTRY_MIN
        } else {
            TASKBAR_ENTRY
        };
        screen.fill(rect, clip, background);
        let accent = if focused == Some(surface.id) {
            BORDER_COLOR_FOCUS
        } else {
            BORDER_COLOR
        };
        screen.fill(
            Rect::new(rect.x, rect.y + rect.h - 2, rect.w, 2),
            clip,
            accent,
        );
        screen.text(
            rect.x + ENTRY_PAD,
            rect.y + (ENTRY_H - display::font::H) / 2,
            &surface.title,
            TITLE_TEXT,
            rect.intersect(clip),
            1,
        );
    });
}

/// Handle one display request; returns the reply parcel for a synchronous call.
#[allow(clippy::too_many_arguments)]
fn handle_request(
    message: &Message,
    surfaces: &mut Vec<Surface>,
    screen: &mut Canvas,
    next_id: &mut u64,
    focused: &mut Option<u64>,
    drag: &mut Option<Drag>,
    pointer: (i32, i32),
    drag_session: &mut Option<DragSession>,
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
            // the row is full, so every surface is visible at once. The right
            // edge comes from the rightmost window, not the top of the paint
            // order (raising reorders `surfaces`).
            let count = surfaces.len() as i32;
            let x = surfaces
                .iter()
                .map(|surface| surface.x + surface.window().w + 16)
                .max()
                .unwrap_or(PAD);
            let x = x.min((screen.width() - width as i32 - 32).max(0));
            let y = PAD + (count / 3) * (height as i32 + TITLE_H + 32);
            let y = y.min((screen.height() - height as i32 - TASKBAR_H - 32).max(0));
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
                minimized: false,
            });
            if focused.is_none() {
                *focused = Some(id);
            }
            // A new surface changes the layout (and the taskbar), so repaint
            // the whole screen.
            let full = Rect::new(0, 0, screen.width(), screen.height());
            repaint(
                screen,
                surfaces,
                pointer,
                *focused,
                full,
                drag_session.as_ref(),
            );
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
                    let full = Rect::new(0, 0, screen.width(), screen.height());
                    repaint(
                        screen,
                        surfaces,
                        pointer,
                        *focused,
                        full,
                        drag_session.as_ref(),
                    );
                    Some(empty_reply(message.method()))
                }
                Err(code) => Some(error_reply(message.method(), -code)),
            }
        }
        method::COMMIT => {
            let id = u64_field(&message.parcel, display::field::SURFACE).unwrap_or(0);
            if let Some(surface) = surfaces.iter().find(|surface| surface.id == id) {
                if surface.minimized {
                    // The pixels are hidden; the minimize repaint already
                    // cleared the screen area. Only the buffer changed.
                    return Some(empty_reply(message.method()));
                }
                let content = surface.content();
                let damage = Rect::new(
                    content.x + u64_field(&message.parcel, display::field::X).unwrap_or(0) as i32,
                    content.y + u64_field(&message.parcel, display::field::Y).unwrap_or(0) as i32,
                    u64_field(&message.parcel, display::field::W).unwrap_or(0) as i32,
                    u64_field(&message.parcel, display::field::H).unwrap_or(0) as i32,
                )
                .intersect(content);
                repaint(
                    screen,
                    surfaces,
                    pointer,
                    *focused,
                    damage,
                    drag_session.as_ref(),
                );
            }
            Some(empty_reply(message.method()))
        }
        method::DESTROY_SURFACE => {
            let id = u64_field(&message.parcel, display::field::SURFACE).unwrap_or(0);
            // A window-manager title-bar drag on the surface ends with it.
            if let Some(active) = *drag {
                if active.id == id {
                    *drag = None;
                }
            }
            // A drag & drop session whose source or hovered target goes away
            // ends now.
            let stranding = drag_session
                .as_ref()
                .is_some_and(|active| active.source == id || active.target == Some(id));
            if stranding {
                drag_cancel(drag_session, surfaces, screen, pointer, *focused, scratch);
            }
            remove_surface(surfaces, id);
            if *focused == Some(id) {
                *focused = topmost_visible(surfaces);
            }
            let full = Rect::new(0, 0, screen.width(), screen.height());
            repaint(
                screen,
                surfaces,
                pointer,
                *focused,
                full,
                drag_session.as_ref(),
            );
            Some(empty_reply(message.method()))
        }
        method::DRAG_START => {
            let id = u64_field(&message.parcel, display::field::SURFACE).unwrap_or(0);
            let token = u64_field(&message.parcel, display::field::TOKEN).unwrap_or(0);
            let mime = string_field(&message.parcel, display::field::MIME).unwrap_or_default();
            if drag_session.is_some() {
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
                drag_session,
                surfaces,
                screen,
                pointer,
                *focused,
                scratch,
                id,
                token,
                mime,
            );
            Some(empty_reply(message.method()))
        }
        method::DRAG_CANCEL => {
            let id = u64_field(&message.parcel, display::field::SURFACE).unwrap_or(0);
            let owns = drag_session
                .as_ref()
                .is_some_and(|active| active.source == id)
                && surface_by_id(surfaces, id).is_some_and(|s| s.owner == message.sender);
            if owns {
                drag_cancel(drag_session, surfaces, screen, pointer, *focused, scratch);
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
    pub const WINDOW_CLOSE: u32 = 10;
    pub const DRAG_START: u32 = 11;
    pub const DRAG_CANCEL: u32 = 12;
    pub const DRAG_ENTER: u32 = 13;
    pub const DRAG_OVER: u32 = 14;
    pub const DRAG_LEAVE: u32 = 15;
    pub const DROP: u32 = 16;
    pub const DRAG_ENDED: u32 = 17;
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
