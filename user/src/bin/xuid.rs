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
//! Issue #167 adds the shell protocol (S5.0), append-only on top of the above:
//!
//! * a `DESKTOP` role on `CreateSurface`: the surface paints at the bottom of
//!   the z-order, above the background colour and below every window, with no
//!   chrome, focus, taskbar entry, or Alt+Tab entry; creating another desktop
//!   replaces the current one;
//! * `ListSurfaces` (one row per surface: id, title, geometry, minimized,
//!   focused), `GetWorkArea` (the window rectangle above the fallback taskbar),
//!   `GetTheme` (the chrome palette), and `Subscribe(role, events)` — the
//!   subscriber receives one-way `SurfaceChanged`, `FocusChanged`, and
//!   `StartMenu` events. A `"shell"` subscriber hides the built-in taskbar and
//!   expands the work area to the whole screen, but the fallback bar (and the
//!   old no-shell sessions) keep working;
//! * global hotkeys: `Alt+Tab` opens a centered window overlay, repeated Tab
//!   cycles the selection, releasing Alt commits; `Ctrl+Esc` and `Super` send
//!   `StartMenu` to the shell; `Alt+F4` sends `WindowClose` to the focused
//!   surface; `Escape` still cancels a drag & drop.
//!
//! Boot it with `LAZYOS_XUID=1`; the kernel starts this program and `xdemo`.
//! The shell-probe evidence client (`shellprobe`) boots too when the
//! `LAZYOS_SHELLPROBE=1` demo hook is set (`SHELLPRB.ELF`).
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
use core::sync::atomic::{AtomicBool, Ordering};
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
/// Alt+Tab overlay panel background and border (issue #167).
const OVERLAY_BG: Color = Color::rgb(20, 24, 38);
const OVERLAY_BORDER: Color = Color::rgb(122, 138, 196);
/// Alt+Tab selected-entry highlight and its text.
const OVERLAY_SELECTED: Color = Color::rgb(44, 112, 74);
const OVERLAY_TEXT: Color = Color::rgb(220, 226, 240);

/// The serial marker the evidence session greps for.
const UP_MARKER: &str = "XUID:UP:PASS\n";
/// The marker that says the window-management features came up.
const WM_MARKER: &str = "XUID:WM:PASS\n";
/// The marker that says the shell-protocol additions came up (issue #167).
const SHELL_MARKER: &str = "XUID:SHELL:PASS\n";

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
    /// The bottom-layer desktop surface (issue #167): no chrome, never
    /// focused, hit-tested, minimized, or listed on the taskbar.
    desktop: bool,
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
    /// Pointer offset from the window origin at grab time.
    grab_y: i32,
}

// ---------------------------------------------------------------------------
// Shell protocol (issue #167)
//
// LazyShell subscribes with Subscribe(role, events) and receives one-way
// SurfaceChanged/FocusChanged/StartMenu events; `"shell"` hides the built-in
// taskbar so the shell owns it. The desktop role is a CreateSurface flag. The
// Alt+Tab overlay is compositor-owned; the shell only sees the resulting
// FocusChanged/SurfaceChanged events.
// ---------------------------------------------------------------------------

/// A registered shell subscriber.
struct ShellSub {
    /// Role string from `Subscribe`; [`display::ROLE_SHELL`] hides the bar.
    role: String,
    /// Event endpoint handle in this task's table.
    events: u64,
}

/// The modifier keys currently held, tracked from forwarded key codes.
#[derive(Clone, Copy, Default)]
struct Modifiers {
    #[allow(dead_code)]
    shift: bool,
    ctrl: bool,
    alt: bool,
    #[allow(dead_code)]
    super_key: bool,
}

/// The open Alt+Tab overlay: a snapshot of the window cycle and the current
/// selection.
struct AltTab {
    /// Visible window ids in cycle order (creation order).
    order: Vec<u64>,
    /// Index into `order` of the highlighted entry.
    selected: usize,
}

/// Whether the built-in taskbar paints: it stays the no-shell fallback and is
/// hidden once a `"shell"` subscriber registers.
fn taskbar_visible(shell: Option<&ShellSub>) -> bool {
    shell
        .map(|shell| shell.role != display::ROLE_SHELL)
        .unwrap_or(true)
}

/// Set by [`notify_shell`] when the subscriber's event endpoint reports
/// `EPIPE` (issue #175): the shell process died without unsubscribing. The
/// main loop checks this after every event/request batch, drops the stale
/// subscription and repaints so the fallback taskbar returns.
static SHELL_DEAD: AtomicBool = AtomicBool::new(false);

/// Send one shell event to the subscriber. A closed peer (`EPIPE`) is
/// recorded in [`SHELL_DEAD`] instead of being silently ignored, so the
/// caller can drop the subscription (issue #175).
fn notify_shell(
    shell: Option<&ShellSub>,
    scratch: &mut Vec<u8>,
    method: u32,
    fields: &[(u16, u64)],
    text: Option<(u16, &str)>,
) {
    let Some(shell) = shell else {
        return;
    };
    let result = display::send_event_fields(
        &Endpoint::from_raw(shell.events),
        scratch,
        method,
        fields,
        text,
    );
    if let Err(messenger::Error::Errno(code)) = result {
        if code == -messenger::errno::EPIPE {
            SHELL_DEAD.store(true, Ordering::Relaxed);
        }
    }
}

/// Tell the shell a surface changed (created/destroyed/moved/minimized/
/// restored). Created events carry the title; every row carries the composited
/// geometry and flags.
fn notify_surface(
    shell: Option<&ShellSub>,
    scratch: &mut Vec<u8>,
    surface: &Surface,
    focused: Option<u64>,
    kind: u64,
) {
    let role = if surface.desktop {
        display::role::DESKTOP
    } else {
        display::role::WINDOW
    };
    let fields = [
        (display::field::SURFACE, surface.id),
        (display::field::A, kind),
        (display::field::X, surface.x.max(0) as u64),
        (display::field::Y, surface.y.max(0) as u64),
        (display::field::W, surface.w.max(0) as u64),
        (display::field::H, surface.h.max(0) as u64),
        (display::field::MINIMIZED, surface.minimized as u64),
        (
            display::field::FOCUSED,
            (focused == Some(surface.id)) as u64,
        ),
        (display::field::ROLE, role),
    ];
    let text = (kind == display::change::CREATED)
        .then_some((display::field::TITLE, surface.title.as_str()));
    notify_shell(shell, scratch, method::SURFACE_CHANGED, &fields, text);
}

/// Tell the shell a surface is gone.
fn notify_destroyed(shell: Option<&ShellSub>, scratch: &mut Vec<u8>, id: u64) {
    notify_shell(
        shell,
        scratch,
        method::SURFACE_CHANGED,
        &[
            (display::field::SURFACE, id),
            (display::field::A, display::change::DESTROYED),
        ],
        None,
    );
}

/// Tell the shell which surface is focused (`None` = none).
fn notify_focus(shell: Option<&ShellSub>, scratch: &mut Vec<u8>, focused: Option<u64>) {
    notify_shell(
        shell,
        scratch,
        method::FOCUS_CHANGED,
        &[(display::field::SURFACE, focused.unwrap_or(0))],
        None,
    );
}

/// Forward the global start-menu hotkey to the shell.
fn notify_start_menu(shell: Option<&ShellSub>, scratch: &mut Vec<u8>) {
    notify_shell(shell, scratch, method::START_MENU, &[], None);
}

/// If a notification since the last check found the shell subscriber's
/// endpoint closed ([`SHELL_DEAD`]), drop the subscription and repaint the
/// full screen so the fallback taskbar returns and `GetWorkArea` reports the
/// full window rectangle again (issue #175).
fn reap_dead_shell(
    shell: &mut Option<ShellSub>,
    surfaces: &[Surface],
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: Option<u64>,
    drag_session: Option<&DragSession>,
    alt_tab: Option<&AltTab>,
) {
    if !SHELL_DEAD.swap(false, Ordering::Relaxed) || shell.take().is_none() {
        return;
    }
    let full = Rect::new(0, 0, screen.width(), screen.height());
    // The subscription is already gone, so the fallback taskbar is visible.
    repaint(
        screen,
        surfaces,
        pointer,
        focused,
        full,
        drag_session,
        true,
        alt_tab,
    );
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
/// `source` and the desktop.
fn drag_target_at(surfaces: &[Surface], source: u64, point: (i32, i32)) -> Option<u64> {
    surfaces
        .iter()
        .rev()
        .find(|surface| {
            !surface.minimized
                && !surface.desktop
                && surface.id != source
                && contains(surface.content(), point)
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
    taskbar: bool,
    alt_tab: Option<&AltTab>,
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
    repaint(
        screen,
        surfaces,
        pointer,
        focused,
        damage,
        drag.as_ref(),
        taskbar,
        alt_tab,
    );
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
#[allow(clippy::too_many_arguments)]
fn drag_finish(
    drag: &mut Option<DragSession>,
    surfaces: &[Surface],
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: Option<u64>,
    scratch: &mut Vec<u8>,
    taskbar: bool,
    alt_tab: Option<&AltTab>,
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
    repaint(
        screen,
        surfaces,
        pointer,
        focused,
        damage,
        drag.as_ref(),
        taskbar,
        alt_tab,
    );
}

/// Cancel a live drag (Escape, `DragCancel`, or the surface going away).
#[allow(clippy::too_many_arguments)]
fn drag_cancel(
    drag: &mut Option<DragSession>,
    surfaces: &[Surface],
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: Option<u64>,
    scratch: &mut Vec<u8>,
    taskbar: bool,
    alt_tab: Option<&AltTab>,
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
    repaint(
        screen,
        surfaces,
        pointer,
        focused,
        damage,
        drag.as_ref(),
        taskbar,
        alt_tab,
    );
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
    // Issue #167: the registered shell subscriber, the held modifiers, and the
    // open Alt+Tab overlay.
    let mut shell: Option<ShellSub> = None;
    let mut mods = Modifiers::default();
    let mut alt_tab: Option<AltTab> = None;
    // One receive buffer and one event-encode buffer for the whole life of the
    // compositor: the user bump allocator never reclaims, so the loop reuses
    // both instead of allocating per message.
    let mut request_buf = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let mut event_scratch = Vec::with_capacity(64);

    // First frame: the previous mux pixels are still on screen, so paint the
    // desktop and present before announcing readiness.
    repaint(
        &mut screen,
        &surfaces,
        pointer,
        focused,
        full,
        None,
        taskbar_visible(shell.as_ref()),
        alt_tab.as_ref(),
    );
    sys::write_str(UP_MARKER);
    sys::write_str(WM_MARKER);
    sys::write_str(SHELL_MARKER);

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
                            shell.as_ref(),
                            &mut mods,
                            &mut alt_tab,
                            &mut consumed,
                        );
                    }
                }
                Err(_) => break,
            }
        }
        reap_dead_shell(
            &mut shell,
            &surfaces,
            &mut screen,
            pointer,
            focused,
            drag_session.as_ref(),
            alt_tab.as_ref(),
        );

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
                        &mut shell,
                        &mut alt_tab,
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
        reap_dead_shell(
            &mut shell,
            &surfaces,
            &mut screen,
            pointer,
            focused,
            drag_session.as_ref(),
            alt_tab.as_ref(),
        );
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

/// Close the endpoint handle that arrived with a request we are rejecting;
/// otherwise every refused request leaks one slot in the compositor's
/// (immortal) handle table.
fn drop_rejected_handle(message: &Message) {
    if message.handles != 0 {
        let _ = Endpoint::from_raw(message.first_handle).close();
    }
}

/// Whether `sender`'s kernel-stamped credentials authorize the compositor's
/// administrative operations (issue #175): claiming the `"shell"` role,
/// replacing the desktop, and listing every surface. Mirrors accountsd's
/// admin check: uid 0, or `CAP_SETUID` for a delegated system service. A
/// refusal or a read error is "not authorized".
fn is_privileged(sender: u64) -> bool {
    let mut cred = sys::Cred::default();
    match sys::cred_get(Some(sender), &mut cred) {
        Ok(()) => cred.uid == 0 || cred.caps & sys::CAP_SETUID != 0,
        Err(_) => false,
    }
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
    shell: Option<&ShellSub>,
    mods: &mut Modifiers,
    alt_tab: &mut Option<AltTab>,
    consumed: &mut u32,
) {
    let (screen_w, screen_h) = (screen.width(), screen.height());
    // `consumed` bit for the button in a press/release event.
    let button_bit = 1u32 << (event.a as u32 & 31);
    let full = Rect::new(0, 0, screen_w, screen_h);
    let bar = taskbar_visible(shell);
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
                    bar,
                    alt_tab.as_ref(),
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
                    bar,
                    alt_tab.as_ref(),
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
                bar,
                alt_tab.as_ref(),
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
            // The fallback taskbar paints above every window, so it hit-tests
            // first; with a shell registered it is hidden and not hit-tested.
            if bar {
                if let Some(id) = taskbar_hit(surfaces, screen_w, screen_h, point) {
                    *consumed |= button_bit;
                    let before = *focused;
                    let was_minimized =
                        surface_by_id(surfaces, id).is_some_and(|surface| surface.minimized);
                    restore(surfaces, focused, id);
                    if was_minimized {
                        if let Some(surface) = surface_by_id(surfaces, id) {
                            notify_surface(
                                shell,
                                scratch,
                                surface,
                                *focused,
                                display::change::RESTORED,
                            );
                        }
                    }
                    if *focused != before {
                        notify_focus(shell, scratch, *focused);
                    }
                    repaint(
                        screen,
                        surfaces,
                        *pointer,
                        *focused,
                        full,
                        drag_session.as_ref(),
                        bar,
                        alt_tab.as_ref(),
                    );
                    return;
                }
            }
            // A press outside every window is a desktop click: ignore it.
            let Some((id, origin, close, minimize, title)) = surfaces
                .iter()
                .rev()
                .find(|surface| {
                    !surface.desktop && !surface.minimized && contains(surface.window(), point)
                })
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
            let before = *focused;
            raise(surfaces, id);
            *focused = Some(id);
            if *focused != before {
                notify_focus(shell, scratch, *focused);
            }
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
                    shell,
                    bar,
                    alt_tab.as_ref(),
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
                    shell,
                    scratch,
                    bar,
                    alt_tab.as_ref(),
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
                    bar,
                    alt_tab.as_ref(),
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
                bar,
                alt_tab.as_ref(),
            );
            // This press goes to the surface, so its release must too: drop a
            // stale consumed bit left by a release the input queue dropped.
            *consumed &= !button_bit;
            let (x, y) = relative(surfaces, id, point);
            forward(surfaces, scratch, Some(id), method::POINTER_DOWN, x, y);
        }
        EventKind::PointerUp => {
            *button_down = false;
            if drag_session.is_some() {
                *consumed &= !button_bit;
                drag_finish(
                    drag_session,
                    surfaces,
                    screen,
                    *pointer,
                    *focused,
                    scratch,
                    bar,
                    alt_tab.as_ref(),
                );
                return;
            }
            if *consumed & button_bit != 0 {
                // The matching press was the compositor's (taskbar, window
                // button, title bar or desktop): swallow its release, and end
                // a title-bar drag it started.
                *consumed &= !button_bit;
                if event.a as u32 == display::button::LEFT {
                    if let Some(active) = drag.take() {
                        // The drag is committed, so tell the shell the new geometry.
                        if let Some(surface) = surface_by_id(surfaces, active.id) {
                            notify_surface(
                                shell,
                                scratch,
                                surface,
                                *focused,
                                display::change::MOVED,
                            );
                        }
                    }
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
            let key = event.a as u32;
            // Modifier keys are compositor-level (issue #167): track them and
            // never forward them to a client.
            if modifier_key(key) {
                match key {
                    display::key::SHIFT => mods.shift = true,
                    display::key::CTRL => mods.ctrl = true,
                    display::key::ALT => mods.alt = true,
                    display::key::SUPER => {
                        mods.super_key = true;
                        notify_start_menu(shell, scratch);
                    }
                    _ => {}
                }
                return;
            }
            if key == display::key::ESCAPE {
                // Escape closes the Alt+Tab overlay first...
                if alt_tab.take().is_some() {
                    repaint(
                        screen,
                        surfaces,
                        *pointer,
                        *focused,
                        full,
                        drag_session.as_ref(),
                        bar,
                        None,
                    );
                    return;
                }
                // ...then it is the Ctrl+Esc start-menu chord...
                if mods.ctrl {
                    notify_start_menu(shell, scratch);
                    return;
                }
                // ...and otherwise it cancels a live drag & drop (issue #145).
                if drag_session.is_some() {
                    drag_cancel(
                        drag_session,
                        surfaces,
                        screen,
                        *pointer,
                        *focused,
                        scratch,
                        bar,
                        alt_tab.as_ref(),
                    );
                    return;
                }
            }
            if key == display::key::TAB {
                if mods.alt {
                    // Alt+Tab: the compositor's own overlay, not a client key.
                    alt_tab_open(alt_tab, surfaces, focused, screen, *pointer, bar);
                } else {
                    let before = *focused;
                    cycle_focus(surfaces, focused);
                    if *focused != before {
                        notify_focus(shell, scratch, *focused);
                    }
                    repaint(
                        screen,
                        surfaces,
                        *pointer,
                        *focused,
                        full,
                        drag_session.as_ref(),
                        bar,
                        alt_tab.as_ref(),
                    );
                }
                return;
            }
            if key == display::key::F4 && mods.alt {
                // Alt+F4: ask the focused window to close, exactly like its X
                // button.
                if let Some(id) = *focused {
                    close_surface(
                        surfaces,
                        screen,
                        *pointer,
                        focused,
                        scratch,
                        drag_session.as_ref(),
                        shell,
                        bar,
                        alt_tab.as_ref(),
                        id,
                    );
                }
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
            let key = event.a as u32;
            if modifier_key(key) {
                match key {
                    display::key::SHIFT => mods.shift = false,
                    display::key::CTRL => mods.ctrl = false,
                    display::key::ALT => {
                        mods.alt = false;
                        // Releasing Alt commits the Alt+Tab selection.
                        if let Some(tab) = alt_tab.take() {
                            alt_tab_commit(
                                &tab, surfaces, focused, screen, *pointer, shell, scratch, bar,
                            );
                        }
                    }
                    display::key::SUPER => mods.super_key = false,
                    _ => {}
                }
                return;
            }
            // The release half of the Ctrl+Esc chord is consumed as well.
            if key == display::key::ESCAPE && mods.ctrl {
                return;
            }
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

/// Whether a key code is one of the modifier keys the compositor consumes.
fn modifier_key(key: u32) -> bool {
    matches!(
        key,
        display::key::SHIFT | display::key::CTRL | display::key::ALT | display::key::SUPER
    )
}

/// Open (or advance) the Alt+Tab overlay. The first Tab snapshots the visible
/// windows and selects the one after the current focus; repeated Tabs cycle.
fn alt_tab_open(
    alt_tab: &mut Option<AltTab>,
    surfaces: &[Surface],
    focused: &Option<u64>,
    screen: &mut Canvas,
    pointer: (i32, i32),
    taskbar: bool,
) {
    match alt_tab {
        Some(tab) if !tab.order.is_empty() => {
            tab.selected = (tab.selected + 1) % tab.order.len();
        }
        _ => {
            let order: Vec<u64> = surfaces
                .iter()
                .filter(|surface| !surface.desktop && !surface.minimized)
                .map(|surface| surface.id)
                .collect();
            if order.is_empty() {
                return;
            }
            let current = focused.and_then(|id| order.iter().position(|&entry| entry == id));
            let selected = match current {
                Some(index) => (index + 1) % order.len(),
                None => 0,
            };
            *alt_tab = Some(AltTab { order, selected });
        }
    }
    let full = Rect::new(0, 0, screen.width(), screen.height());
    repaint(
        screen,
        surfaces,
        pointer,
        *focused,
        full,
        None,
        taskbar,
        alt_tab.as_ref(),
    );
}

/// Commit the Alt+Tab selection: restore, raise, and focus it, and tell the
/// shell about the resulting state changes.
#[allow(clippy::too_many_arguments)]
fn alt_tab_commit(
    tab: &AltTab,
    surfaces: &mut Vec<Surface>,
    focused: &mut Option<u64>,
    screen: &mut Canvas,
    pointer: (i32, i32),
    shell: Option<&ShellSub>,
    scratch: &mut Vec<u8>,
    taskbar: bool,
) {
    let Some(id) = tab.order.get(tab.selected).copied() else {
        return;
    };
    if surface_by_id(surfaces, id).is_none() {
        return;
    }
    let before = *focused;
    let was_minimized = surface_by_id(surfaces, id).is_some_and(|surface| surface.minimized);
    restore(surfaces, focused, id);
    if was_minimized {
        if let Some(surface) = surface_by_id(surfaces, id) {
            notify_surface(shell, scratch, surface, *focused, display::change::RESTORED);
        }
    }
    if *focused != before {
        notify_focus(shell, scratch, *focused);
    }
    let full = Rect::new(0, 0, screen.width(), screen.height());
    repaint(
        screen, surfaces, pointer, *focused, full, None, taskbar, None,
    );
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

/// The topmost visible window's id (desktops are never focusable).
fn topmost_visible(surfaces: &[Surface]) -> Option<u64> {
    surfaces
        .iter()
        .rev()
        .find(|surface| !surface.desktop && !surface.minimized)
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
#[allow(clippy::too_many_arguments)]
fn minimize_surface(
    surfaces: &mut [Surface],
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: &mut Option<u64>,
    drag_session: Option<&DragSession>,
    shell: Option<&ShellSub>,
    scratch: &mut Vec<u8>,
    taskbar: bool,
    alt_tab: Option<&AltTab>,
    id: u64,
) {
    if let Some(surface) = surfaces.iter_mut().find(|surface| surface.id == id) {
        surface.minimized = true;
    }
    if *focused == Some(id) {
        *focused = topmost_visible(surfaces);
        notify_focus(shell, scratch, *focused);
    }
    // The row carries the post-minimize focus flag, so send it after the focus
    // recompute.
    if let Some(surface) = surface_by_id(surfaces, id) {
        notify_surface(
            shell,
            scratch,
            surface,
            *focused,
            display::change::MINIMIZED,
        );
    }
    let full = Rect::new(0, 0, screen.width(), screen.height());
    repaint(
        screen,
        surfaces,
        pointer,
        *focused,
        full,
        drag_session,
        taskbar,
        alt_tab,
    );
}

/// Close a surface: tell the client through a one-way `WindowClose` event and
/// drop it; the full-screen repaint lets the windows below show through.
#[allow(clippy::too_many_arguments)]
fn close_surface(
    surfaces: &mut Vec<Surface>,
    screen: &mut Canvas,
    pointer: (i32, i32),
    focused: &mut Option<u64>,
    scratch: &mut Vec<u8>,
    drag_session: Option<&DragSession>,
    shell: Option<&ShellSub>,
    taskbar: bool,
    alt_tab: Option<&AltTab>,
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
    notify_destroyed(shell, scratch, id);
    remove_surface(surfaces, id);
    if *focused == Some(id) {
        *focused = topmost_visible(surfaces);
        notify_focus(shell, scratch, *focused);
    }
    let full = Rect::new(0, 0, screen.width(), screen.height());
    repaint(
        screen,
        surfaces,
        pointer,
        *focused,
        full,
        drag_session,
        taskbar,
        alt_tab,
    );
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
    if surfaces
        .iter()
        .filter(|surface| !surface.desktop)
        .all(|surface| surface.minimized)
    {
        *focused = None;
        return;
    }
    let current_id = *focused;
    if let Some(current) = current_id.and_then(|id| surfaces.iter().position(|s| s.id == id)) {
        for step in 1..=surfaces.len() {
            let index = (current + step) % surfaces.len();
            if !surfaces[index].desktop
                && !surfaces[index].minimized
                && Some(surfaces[index].id) != current_id
            {
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
        .find(|surface| !surface.desktop && !surface.minimized)
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
    while let Some(surface) = surfaces
        .iter()
        .filter(|surface| !surface.desktop && surface.id > last_id)
        .min_by_key(|surface| surface.id)
    {
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

/// Compose `damage` from the background, the desktop surface, every visible
/// window in z-order, the fallback taskbar, the Alt+Tab overlay, the active
/// drag & drop session (if any), and the cursor, then present exactly that
/// rectangle.
#[allow(clippy::too_many_arguments)]
fn repaint(
    screen: &mut Canvas,
    surfaces: &[Surface],
    pointer: (i32, i32),
    focused: Option<u64>,
    damage: Rect,
    drag_session: Option<&DragSession>,
    taskbar: bool,
    alt_tab: Option<&AltTab>,
) {
    if damage.is_empty() {
        return;
    }
    screen.fill(damage, damage, BACKGROUND);
    // The desktop paints above the background and below every window.
    if let Some(desktop) = surfaces.iter().find(|surface| surface.desktop) {
        draw_desktop(screen, desktop, damage);
    }
    for surface in surfaces
        .iter()
        .filter(|surface| !surface.desktop && !surface.minimized)
    {
        draw_surface(screen, surface, focused == Some(surface.id), damage);
    }
    if taskbar {
        draw_taskbar(screen, surfaces, focused, damage);
    }
    if let Some(session) = drag_session {
        draw_drag(screen, surfaces, session, pointer, damage);
    }
    if let Some(tab) = alt_tab {
        draw_alt_tab(screen, surfaces, tab, damage);
    }
    screen.cursor(pointer.0, pointer.1, damage);
    let _ = sys::display_present(damage.x, damage.y, damage.w, damage.h);
}

/// Blit the desktop surface's pixels across its rectangle; no chrome, no
/// fallback placeholder text (a desktop without pixels is just the background).
fn draw_desktop(screen: &mut Canvas, surface: &Surface, clip: Rect) {
    let area = Rect::new(surface.x, surface.y, surface.w, surface.h);
    if area.intersect(clip).is_empty() {
        return;
    }
    if surface.pixels != 0 && surface.bytes >= (surface.w * surface.h * 4) as u64 {
        // Safety: the mapping was installed by `display_map_buffer` for this
        // buffer and the surface's geometry describes it.
        let pixels = unsafe {
            core::slice::from_raw_parts(surface.pixels as *const u8, surface.bytes as usize)
        };
        screen.blit(pixels, surface.w, surface.h, area, clip);
    }
}

/// Draw the Alt+Tab overlay centered on the screen: one row per window in the
/// cycle, the selected row highlighted.
fn draw_alt_tab(screen: &mut Canvas, surfaces: &[Surface], tab: &AltTab, clip: Rect) {
    let (screen_w, screen_h) = (screen.width(), screen.height());
    let rows = tab.order.len().min(12);
    if rows == 0 {
        return;
    }
    let title_w = tab
        .order
        .iter()
        .filter_map(|id| surface_by_id(surfaces, *id))
        .map(|surface| surface.title.chars().count() as i32 * display::font::ADVANCE)
        .max()
        .unwrap_or(0);
    let panel_w = (title_w + 48).clamp(180, (screen_w - 40).max(180));
    let row_h = 18;
    let panel_h = 26 + rows as i32 * row_h;
    let panel = Rect::new(
        (screen_w - panel_w) / 2,
        (screen_h - panel_h) / 2,
        panel_w,
        panel_h,
    );
    if panel.intersect(clip).is_empty() {
        return;
    }
    screen.fill(panel, clip, OVERLAY_BG);
    screen.fill(
        Rect::new(panel.x, panel.y, panel.w, 2),
        clip,
        OVERLAY_BORDER,
    );
    screen.fill(
        Rect::new(panel.x, panel.y + panel.h - 2, panel.w, 2),
        clip,
        OVERLAY_BORDER,
    );
    screen.fill(
        Rect::new(panel.x, panel.y, 2, panel.h),
        clip,
        OVERLAY_BORDER,
    );
    screen.fill(
        Rect::new(panel.x + panel.w - 2, panel.y, 2, panel.h),
        clip,
        OVERLAY_BORDER,
    );
    screen.text(panel.x + 10, panel.y + 8, "alt+tab", OVERLAY_TEXT, clip, 1);
    // Highlight the selected row before its text, then paint the titles.
    for (index, id) in tab.order.iter().take(rows).enumerate() {
        let row = Rect::new(
            panel.x + 6,
            panel.y + 22 + index as i32 * row_h,
            panel.w - 12,
            row_h - 2,
        );
        if index == tab.selected {
            screen.fill(row, clip, OVERLAY_SELECTED);
        }
        if let Some(surface) = surface_by_id(surfaces, *id) {
            screen.text(
                row.x + 8,
                row.y + (row.h - display::font::H) / 2,
                &surface.title,
                OVERLAY_TEXT,
                row.intersect(clip),
                1,
            );
        }
    }
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
    shell: &mut Option<ShellSub>,
    alt_tab: &mut Option<AltTab>,
) -> Option<Parcel> {
    if message.interface_id() != display::INTERFACE {
        return Some(empty_reply(message.method()));
    }
    let bar = taskbar_visible(shell.as_ref());
    match message.method() {
        method::CREATE_SURFACE => {
            let width = u64_field(&message.parcel, display::field::WIDTH).unwrap_or(0);
            let height = u64_field(&message.parcel, display::field::HEIGHT).unwrap_or(0);
            let title = string_field(&message.parcel, display::field::TITLE)
                .unwrap_or_else(|| String::from("app"));
            let role =
                u64_field(&message.parcel, display::field::ROLE).unwrap_or(display::role::WINDOW);
            // A window can never exceed the screen anyway, and bounding it
            // here keeps `width * height * 4` well inside `i32` downstream
            // (issue #176: an unbounded claim let that multiplication wrap).
            let (max_w, max_h) = (screen.width().max(0) as u64, screen.height().max(0) as u64);
            if width == 0 || height == 0 || width > max_w || height > max_h || message.handles == 0
            {
                drop_rejected_handle(&message);
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            }
            if role == display::role::DESKTOP && !is_privileged(message.sender) {
                // Only an authorized shell identity may own the desktop
                // (issue #175); anyone else's claim is refused outright.
                drop_rejected_handle(&message);
                return Some(error_reply(message.method(), messenger::errno::EACCES));
            }
            let id = *next_id;
            *next_id += 1;
            let full = Rect::new(0, 0, screen.width(), screen.height());
            if role == display::role::DESKTOP {
                // The bottom layer: no chrome, no taskbar entry, never focused.
                // A new desktop replaces the current one.
                if let Some(index) = surfaces.iter().position(|surface| surface.desktop) {
                    let old = surfaces.remove(index);
                    // Tell the old owner and close the endpoint the
                    // compositor held for it (issue #175: both were leaked).
                    let _ = display::send_event(
                        &Endpoint::from_raw(old.events),
                        scratch,
                        method::WINDOW_CLOSE,
                        0,
                        0,
                    );
                    notify_destroyed(shell.as_ref(), scratch, old.id);
                    let _ = Endpoint::from_raw(old.events).close();
                }
                surfaces.push(Surface {
                    id,
                    title,
                    x: 0,
                    y: 0,
                    w: width as i32,
                    h: height as i32,
                    events: message.first_handle,
                    owner: message.sender,
                    pixels: 0,
                    bytes: 0,
                    minimized: false,
                    desktop: true,
                });
                if let Some(surface) = surface_by_id(surfaces, id) {
                    notify_surface(
                        shell.as_ref(),
                        scratch,
                        surface,
                        *focused,
                        display::change::CREATED,
                    );
                }
                repaint(
                    screen,
                    surfaces,
                    pointer,
                    *focused,
                    full,
                    drag_session.as_ref(),
                    bar,
                    alt_tab.as_ref(),
                );
                let mut body = Encoder::new();
                let _ = body.u64(display::field::SURFACE, id);
                return Some(reply_parcel(message.method(), body));
            }
            // Lay windows out left to right at the top, cascading down when
            // the row is full, so every surface is visible at once. The right
            // edge comes from the rightmost window, not the top of the paint
            // order (raising reorders `surfaces`).
            let count = surfaces.iter().filter(|surface| !surface.desktop).count() as i32;
            let x = surfaces
                .iter()
                .filter(|surface| !surface.desktop)
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
                desktop: false,
            });
            let before = *focused;
            if focused.is_none() {
                *focused = Some(id);
            }
            if *focused != before {
                notify_focus(shell.as_ref(), scratch, *focused);
            }
            if let Some(surface) = surface_by_id(surfaces, id) {
                notify_surface(
                    shell.as_ref(),
                    scratch,
                    surface,
                    *focused,
                    display::change::CREATED,
                );
            }
            // A new surface changes the layout (and the taskbar), so repaint
            // the whole screen.
            repaint(
                screen,
                surfaces,
                pointer,
                *focused,
                full,
                drag_session.as_ref(),
                bar,
                alt_tab.as_ref(),
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
            if surface.owner != message.sender {
                // Only the surface's own client may attach its pixels
                // (issue #176: any caller that guessed the id could spoof
                // another app's window).
                return Some(error_reply(message.method(), messenger::errno::EACCES));
            }
            // The descriptor's length is the sender's claim about how many
            // bytes the surface needs; never trust it to cover the geometry
            // the compositor paints. Checked `u64` arithmetic avoids the
            // wrap a pathological width/height could otherwise cause in the
            // `i32` product (issue #176); `CREATE_SURFACE` also bounds both
            // to the screen size, so this is defense in depth.
            let Some(expected) = (surface.w.max(0) as u64)
                .checked_mul(surface.h.max(0) as u64)
                .and_then(|area| area.checked_mul(4))
            else {
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            };
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
                        bar,
                        alt_tab.as_ref(),
                    );
                    Some(empty_reply(message.method()))
                }
                Err(code) => Some(error_reply(message.method(), -code)),
            }
        }
        method::COMMIT => {
            let id = u64_field(&message.parcel, display::field::SURFACE).unwrap_or(0);
            if let Some(surface) = surfaces.iter().find(|surface| surface.id == id) {
                if surface.owner != message.sender {
                    // Only the owner may commit damage (issue #176: any
                    // caller that guessed the id could paint over it).
                    return Some(error_reply(message.method(), messenger::errno::EACCES));
                }
                if surface.minimized {
                    // The pixels are hidden; the minimize repaint already
                    // cleared the screen area. Only the buffer changed.
                    return Some(empty_reply(message.method()));
                }
                // A window's damage is relative to its content origin; the
                // desktop has no chrome, so its origin is the surface origin.
                let area = if surface.desktop {
                    Rect::new(surface.x, surface.y, surface.w, surface.h)
                } else {
                    surface.content()
                };
                let damage = Rect::new(
                    area.x + u64_field(&message.parcel, display::field::X).unwrap_or(0) as i32,
                    area.y + u64_field(&message.parcel, display::field::Y).unwrap_or(0) as i32,
                    u64_field(&message.parcel, display::field::W).unwrap_or(0) as i32,
                    u64_field(&message.parcel, display::field::H).unwrap_or(0) as i32,
                )
                .intersect(area);
                repaint(
                    screen,
                    surfaces,
                    pointer,
                    *focused,
                    damage,
                    drag_session.as_ref(),
                    bar,
                    alt_tab.as_ref(),
                );
            }
            Some(empty_reply(message.method()))
        }
        method::DESTROY_SURFACE => {
            let id = u64_field(&message.parcel, display::field::SURFACE).unwrap_or(0);
            if surface_by_id(surfaces, id).is_some_and(|surface| surface.owner != message.sender) {
                // Only the owner may destroy its own surface (issue #176:
                // any caller that guessed the id could close another app's
                // window).
                return Some(error_reply(message.method(), messenger::errno::EACCES));
            }
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
                drag_cancel(
                    drag_session,
                    surfaces,
                    screen,
                    pointer,
                    *focused,
                    scratch,
                    bar,
                    alt_tab.as_ref(),
                );
            }
            notify_destroyed(shell.as_ref(), scratch, id);
            if let Some(tab) = alt_tab.as_mut() {
                // The Alt+Tab snapshot may not outlive the surface.
                tab.order.retain(|&entry| entry != id);
                if tab.order.is_empty() {
                    *alt_tab = None;
                } else if tab.selected >= tab.order.len() {
                    tab.selected = 0;
                }
            }
            remove_surface(surfaces, id);
            if *focused == Some(id) {
                *focused = topmost_visible(surfaces);
                notify_focus(shell.as_ref(), scratch, *focused);
            }
            let full = Rect::new(0, 0, screen.width(), screen.height());
            repaint(
                screen,
                surfaces,
                pointer,
                *focused,
                full,
                drag_session.as_ref(),
                bar,
                alt_tab.as_ref(),
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
                bar,
                alt_tab.as_ref(),
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
                drag_cancel(
                    drag_session,
                    surfaces,
                    screen,
                    pointer,
                    *focused,
                    scratch,
                    bar,
                    alt_tab.as_ref(),
                );
            }
            Some(empty_reply(message.method()))
        }
        method::SUBSCRIBE => {
            let role =
                string_field(&message.parcel, display::field::SUBSCRIBER_ROLE).unwrap_or_default();
            if message.handles == 0 || role.is_empty() || role.len() > display::MAX_ROLE {
                drop_rejected_handle(&message);
                return Some(error_reply(message.method(), messenger::errno::EINVAL));
            }
            if role == display::ROLE_SHELL && !is_privileged(message.sender) {
                // Only an authorized shell identity may hide the fallback
                // taskbar and receive every surface/focus event (issue
                // #175); anyone else's claim is refused outright.
                drop_rejected_handle(&message);
                return Some(error_reply(message.method(), messenger::errno::EACCES));
            }
            // One subscriber at a time; a re-subscribe replaces the
            // endpoint, so close the one it replaces (issue #175: it was
            // leaked).
            let previous = shell.replace(ShellSub {
                role,
                events: message.first_handle,
            });
            if let Some(previous) = previous {
                let _ = Endpoint::from_raw(previous.events).close();
            }
            let full = Rect::new(0, 0, screen.width(), screen.height());
            repaint(
                screen,
                surfaces,
                pointer,
                *focused,
                full,
                drag_session.as_ref(),
                taskbar_visible(shell.as_ref()),
                alt_tab.as_ref(),
            );
            Some(empty_reply(message.method()))
        }
        method::LIST_SURFACES => {
            if !is_privileged(message.sender) {
                // Every window's title and geometry is compositor-privileged
                // (issue #175); anyone else's request is refused outright.
                return Some(error_reply(message.method(), messenger::errno::EACCES));
            }
            let mut body = Encoder::new();
            // One row per surface, in z-order: `SURFACE` starts a row and the
            // trailing fields describe it.
            for surface in surfaces.iter() {
                let role = if surface.desktop {
                    display::role::DESKTOP
                } else {
                    display::role::WINDOW
                };
                let _ = body.u64(display::field::SURFACE, surface.id);
                let _ = body.string(display::field::TITLE, &surface.title);
                let _ = body.u64(display::field::X, surface.x.max(0) as u64);
                let _ = body.u64(display::field::Y, surface.y.max(0) as u64);
                let _ = body.u64(display::field::W, surface.w.max(0) as u64);
                let _ = body.u64(display::field::H, surface.h.max(0) as u64);
                let _ = body.u64(display::field::MINIMIZED, surface.minimized as u64);
                let _ = body.u64(
                    display::field::FOCUSED,
                    (*focused == Some(surface.id)) as u64,
                );
                let _ = body.u64(display::field::ROLE, role);
            }
            Some(reply_parcel(message.method(), body))
        }
        method::GET_WORK_AREA => {
            // With a shell registered the fallback bar is hidden, so windows
            // may use the whole screen.
            let height = if taskbar_visible(shell.as_ref()) {
                (screen.height() - TASKBAR_H).max(0)
            } else {
                screen.height()
            };
            let mut body = Encoder::new();
            let _ = body.u64(display::field::X, 0);
            let _ = body.u64(display::field::Y, 0);
            let _ = body.u64(display::field::W, screen.width().max(0) as u64);
            let _ = body.u64(display::field::H, height as u64);
            Some(reply_parcel(message.method(), body))
        }
        method::GET_THEME => {
            let mut body = Encoder::new();
            let _ = body.u64(display::field::TITLE_BG_ACTIVE, color_u64(TITLE_BG_FOCUS));
            let _ = body.u64(display::field::TITLE_BG_INACTIVE, color_u64(TITLE_BG));
            let _ = body.u64(display::field::BORDER, color_u64(BORDER_COLOR));
            let _ = body.u64(display::field::TASKBAR, color_u64(TASKBAR_BG));
            let _ = body.u64(display::field::TEXT, color_u64(TITLE_TEXT));
            Some(reply_parcel(message.method(), body))
        }
        _ => Some(error_reply(message.method(), messenger::errno::EINVAL)),
    }
}

/// Pack a colour into the `0xRRGGBB` form `GetTheme` reports.
fn color_u64(color: Color) -> u64 {
    ((color.r as u64) << 16) | ((color.g as u64) << 8) | color.b as u64
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
    pub const LIST_SURFACES: u32 = 18;
    pub const GET_WORK_AREA: u32 = 19;
    pub const SUBSCRIBE: u32 = 20;
    pub const GET_THEME: u32 = 21;
    pub const SURFACE_CHANGED: u32 = 22;
    pub const FOCUS_CHANGED: u32 = 23;
    pub const START_MENU: u32 = 24;
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
