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
//!   subsequent pointer movement moves the origin. A window may be pushed
//!   partly off the left, right and bottom edges; [`geometry::keep_reachable`]
//!   keeps enough of its title bar on screen to grab it again;
//! * a resizable window (one that called `SetSizeHints`, method 32) can be
//!   resized by dragging an edge or corner: the compositor draws a wireframe
//!   outline during the drag and applies the size, via a one-way `Configure`
//!   event (method 33), on release;
//! * the title bar carries close (`X`, asks the client to exit via a one-way
//!   `WindowClose` event), minimize (`-`, hides the surface) and — for a
//!   resizable window — maximize/restore buttons. A double-click on the title
//!   bar toggles maximize; the transition uses the same wireframe zoom as
//!   minimize;
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
//! the union of the old and new window rectangles). Inside the damage only
//! pixels no opaque layer above would overwrite are painted: windows fully
//! hidden by a window, the taskbar, the Alt+Tab panel or the menu are skipped
//! (issue #360, `region.rs`). All state lives in one `Compositor` (`compositor.rs`).
//! The app buffer handoff is already zero-copy
//! (the compositor reads the same frames the app writes); fences and double
//! buffering are the S8 follow-up that turns `Commit` into a tear-free
//! pipeline. Each buffer mapping records the content size it was attached for,
//! so a window resized between the `Configure` and the client's next attach is
//! drawn from the old buffer cropped or padded with the window background,
//! never read with the wrong stride.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "xuid/anim.rs"]
mod anim;
#[path = "xuid/clock.rs"]
mod clock;
#[path = "xuid/compositor.rs"]
mod compositor;
#[path = "xuid/drag.rs"]
mod drag;
#[path = "xuid/event.rs"]
mod event;
#[path = "xuid/geometry.rs"]
mod geometry;
#[path = "xuid/icons.rs"]
mod icons;
#[path = "xuid/inputlink.rs"]
mod inputlink;
#[path = "xuid/keys.rs"]
mod keys;
#[path = "xuid/layout.rs"]
mod layout;
#[path = "xuid/maximize.rs"]
mod maximize;
#[path = "xuid/menu.rs"]
mod menu;
#[path = "xuid/menuitems.rs"]
mod menuitems;
#[path = "xuid/origin.rs"]
mod origin;
#[path = "xuid/pointer_feed.rs"]
mod pointer_feed;
#[path = "xuid/powerfeed.rs"]
mod powerfeed;
#[path = "xuid/powermenu.rs"]
mod powermenu;
#[path = "xuid/present.rs"]
mod present;
#[path = "xuid/protocol.rs"]
mod protocol;
#[path = "xuid/reap.rs"]
mod reap;
#[path = "xuid/region.rs"]
mod region;
#[path = "xuid/render.rs"]
mod render;
#[path = "xuid/request.rs"]
mod request;
#[path = "xuid/request_shell.rs"]
mod request_shell;
#[path = "xuid/resize.rs"]
mod resize;
#[path = "xuid/shell.rs"]
mod shell;
#[path = "xuid/surface.rs"]
mod surface;
#[path = "xuid/theme.rs"]
mod theme;
#[path = "xuid/themefeed.rs"]
mod themefeed;
#[path = "xuid/title.rs"]
mod title;
#[path = "xuid/wheel.rs"]
mod wheel;
#[path = "xuid/window.rs"]
mod window;

use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::messenger::display::{self, Canvas};
use user::messenger::{self, registry};
use user::sys;

use compositor::Compositor;
use protocol::{decode_event, push_coalesced, Event};

/// The serial marker the evidence session greps for.
const UP_MARKER: &str = "XUID:UP:PASS\n";
/// The marker that says the window-management features came up.
const WM_MARKER: &str = "XUID:WM:PASS\n";
/// The marker that says the shell-protocol additions came up (issue #167).
const SHELL_MARKER: &str = "XUID:SHELL:PASS\n";

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
    // Safety: `va`/`size` come from the display bind and describe an RGBA8
    // screen buffer mapped in this task.
    let screen = unsafe { Canvas::new(info.va, screen_w, screen_h) };
    let mut comp = Compositor::new(screen);
    // One receive buffer and one input batch for the whole life of the
    // compositor: the user bump allocator never reclaims, so the loop reuses
    // them instead of allocating per message.
    let mut request_buf = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let mut input: Vec<Event> = Vec::with_capacity(MAX_INPUT);

    // First frame: the previous mux pixels are still on screen, so paint the
    // desktop and present before announcing readiness.
    comp.repaint_full();
    sys::write_str(UP_MARKER);
    sys::write_str(WM_MARKER);
    sys::write_str(SHELL_MARKER);
    sys::write_str(keys::selftest_key_encoding());
    sys::write_str(title::selftest_titles());
    sys::write_str(window::selftest_focus_on_create());
    sys::write_str(origin::selftest_open_origin());
    sys::write_str(geometry::selftest_geometry());
    sys::write_str(anim::selftest_anim());
    sys::write_str(wheel::selftest_wheel_routing());
    sys::write_str(reap::selftest_reap());
    sys::write_str(pointer_feed::selftest_pointer_feed());

    loop {
        // 0. `inputd`: register new surfaces, report focus, apply the
        //    sessions it opened (keys for those windows come from it).
        comp.sync_input();

        // 1. Input: drain the whole kernel queue first so pointer moves
        //    coalesce across poll calls, then handle what is left in order.
        drain_input(&mut input, comp.input.owns_pointer);
        for event in input.drain(..) {
            comp.handle_event(event);
        }
        comp.reap_dead_shell();
        comp.tick_clock();
        comp.tick_theme();
        comp.tick_power();
        comp.reap_dead_surfaces(sys::clock());

        // 2. Requests: serve one, then loop (the deadline bounds the nap when
        //    nothing is pending, keeping input latency at a couple of ticks).
        let deadline = Some(sys::clock() + 2);
        match server.recv_with(&mut request_buf, deadline) {
            Ok(message) => {
                if let Some(txn) = message.txn {
                    let reply = comp.handle_request(&message);
                    let _ = server.reply(txn, &reply);
                } else if message.interface_id() == display::INTERFACE
                    && message.method() == display::wire::METHOD_PRESENT
                {
                    // `Present` (issue #361) is one-way: no reply, only the
                    // `BufferRelease`/`FrameDone` events. Any other message
                    // without a txn is dropped as before.
                    comp.present(&message);
                }
            }
            Err(error) if is_timeout(error) => {}
            Err(messenger::Error::Errno(code)) if code == -messenger::errno::EPIPE => {
                // The client end went away; keep compositing for the others.
            }
            Err(_) => {}
        }
        comp.reap_dead_shell();
    }
}

/// Kernel input records fetched per `display_input_poll` call.
const INPUT_BATCH: usize = 32;
/// The size of one kernel input record.
const EVENT_BYTES: usize = 16;
/// The most kernel records one drain reads before `xuid` handles them (the
/// kernel's own queue bound). The bound counts records read, not events kept:
/// coalescing keeps a run of pointer moves at one entry, so a pointer moving
/// as fast as the queue is drained cannot keep this loop from returning to
/// dispatch input and serve requests. The batch is allocated once, since the
/// user bump allocator never reclaims.
const MAX_INPUT: usize = 256;

/// Drain the kernel input queue into `batch`, collapsing each run of pointer
/// moves into its last record (issue #339). Draining before handling adds no
/// latency and lets a run split across two poll calls still collapse;
/// [`MAX_INPUT`] records bound one drain. While `inputd` owns the pointer
/// (`pointer_feed.rs`) the kernel's pointer records are read and dropped, so
/// only keys come from this stream.
fn drain_input(batch: &mut Vec<Event>, inputd_pointer: bool) {
    let mut records = [0u8; EVENT_BYTES * INPUT_BATCH];
    let mut read = 0;
    while read + INPUT_BATCH <= MAX_INPUT {
        let Ok(count) = sys::display_input_poll(&mut records) else {
            break;
        };
        if count == 0 {
            break;
        }
        read += count;
        for index in 0..count {
            if let Some(event) = decode_event(&records, index) {
                if inputd_pointer && event.kind.is_pointer() {
                    continue;
                }
                push_coalesced(batch, event);
            }
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
