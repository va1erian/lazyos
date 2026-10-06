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
//! * the `surfaces` vector *is* the z-order of the windows — its tail paints
//!   last, and a click raises that window to the tail and focuses it;
//! * the title bar is a drag handle: a left press on it grabs the window and
//!   subsequent pointer movement moves the origin. A window may be pushed
//!   partly off the left, right and bottom edges; [`geometry::keep_reachable`]
//!   keeps enough of its title bar inside the work area to grab it again;
//! * a resizable window (one that called `SetSizeHints`, method 32) can be
//!   resized by dragging an edge or corner: the compositor draws a wireframe
//!   outline during the drag and applies the size, via a one-way `Configure`
//!   event (method 33), on release;
//! * the title bar carries close (`X`, asks the client to exit via a one-way
//!   `WindowClose` event), minimize (`-`, hides the surface) and — for a
//!   resizable window — maximize/restore buttons. A double-click on the title
//!   bar toggles maximize; the transition uses the same wireframe zoom as
//!   minimize. The zooms read input every frame, so the cursor keeps moving
//!   and no event is lost (`held.rs`);
//! * `Tab` cycles focus across visible windows only, skipping minimized ones.
//!
//! Issue #145 adds compositor-mediated drag & drop (methods 11–17): a source
//! hands over a clipboard token with `DragStart`, the compositor owns the
//! pointer while the button is held, and the surface under it receives
//! `DragEnter`/`DragOver`/`DragLeave` and finally `Drop` (with the token) or a
//! cancelled `DragEnded`. Escape cancels. See `drag.rs` and
//! `docs/architecture/display.md`.
//!
//! The desktop itself — taskbar, clock, start menu, wallpaper — is not here:
//! it is LazyShell, a client (issue #157). `xuid` keeps the mechanism and the
//! policy the shell builds on (issues #167, #157, #447):
//!
//! * `Subscribe("shell", events)` makes a task *the* shell (privileged, or
//!   the session that owns the display; a restart from that session replaces
//!   it). It receives one-way `SurfaceChanged`, `FocusChanged`, `StartMenu`
//!   and `Dismiss` events. Other roles are a privileged observer slot that
//!   never displaces the shell;
//! * the shell alone may create the `Desktop` surface (bottom layer) and
//!   `Panel` surfaces (chromeless, above every window, placed with
//!   `PlaceSurface`), list surfaces, and activate, minimize, set the work
//!   area, the icon geometry and the launch origin (methods 36–41). Panels and
//!   the desktop get the pointer while it is over them (`layers.rs`);
//! * global hotkeys: `Alt+Tab` opens a centered window overlay (minimized
//!   windows included, restored on commit), repeated Tab cycles the
//!   selection, releasing Alt commits; `Ctrl+Esc` and `Super` send
//!   `StartMenu` to the shell; `Alt+F4` sends `WindowClose` to the focused
//!   surface; `Escape` still cancels a drag & drop. With no shell, `xuid`
//!   paints the background and windows, and Alt+Tab reaches every window.
//!
//! Boot it with `LAZYOS_XUID=1`; the kernel starts this program and `xdemo`.
//! The shell-probe evidence client (`shellprobe`) boots too when the
//! `LAZYOS_SHELLPROBE=1` demo hook is set (`/system/bin/shellprobe`).
//!
//! The compositing model is deliberately simple: one screen-sized RGBA buffer
//! and rectangle damage. `Commit` copies the app's damaged rectangle into the
//! screen buffer and presents exactly that rectangle; window-management events
//! are layout changes and repaint the full screen (a title-bar drag repaints
//! the union of the old and new window rectangles). Inside the damage only
//! pixels no opaque layer above would overwrite are painted: windows fully
//! hidden by a window, a panel or the Alt+Tab panel are skipped (issue #360,
//! `region.rs`). All state lives in one `Compositor` (`compositor.rs`).
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
#[path = "xuid/compositor.rs"]
mod compositor;
#[path = "xuid/cursor.rs"]
mod cursor;
#[path = "xuid/drag.rs"]
mod drag;
#[path = "xuid/event.rs"]
mod event;
#[path = "xuid/geometry.rs"]
mod geometry;
#[path = "xuid/grabs.rs"]
mod grabs;
#[path = "xuid/held.rs"]
mod held;
#[path = "xuid/icons.rs"]
mod icons;
#[path = "xuid/inputlink.rs"]
mod inputlink;
#[path = "xuid/keys.rs"]
mod keys;
#[path = "xuid/layers.rs"]
mod layers;
#[path = "xuid/layout.rs"]
mod layout;
#[path = "xuid/maximize.rs"]
mod maximize;
#[path = "xuid/opening.rs"]
mod opening;
#[path = "xuid/origin.rs"]
mod origin;
#[path = "xuid/pointer_feed.rs"]
mod pointer_feed;
#[path = "xuid/loginfeed.rs"]
mod loginfeed;
#[path = "xuid/powerfeed.rs"]
mod powerfeed;
#[path = "xuid/present.rs"]
mod present;
#[path = "xuid/probe.rs"]
mod probe;
#[path = "xuid/protocol.rs"]
mod protocol;
#[cfg(lazyos_desktop)]
#[path = "xuid/provisioning.rs"]
mod provisioning;
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
#[path = "xuid/shellcalls.rs"]
mod shellcalls;
#[path = "xuid/spinner.rs"]
mod spinner;
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
use user::messenger::{self, registry, wait};
use user::sys;

use compositor::Compositor;
use protocol::{push_coalesced, Event};

/// The serial marker the evidence session greps for.
const UP_MARKER: &str = "XUID:UP:PASS\n";
/// The marker that says the window-management features came up.
const WM_MARKER: &str = "XUID:WM:PASS\n";
/// The marker that says the shell-protocol additions came up (issue #167).
const SHELL_MARKER: &str = "XUID:SHELL:PASS\n";
/// Longest park while nothing is due (100 Hz ticks): requests, `inputd`'s
/// pointer events and keys wake the loop themselves (docs/performance-plan.md
/// P1.4), so this only paces the pull feeds (theme, power) and reaping.
const IDLE_TICKS: u64 = 10;
/// The park when the wait itself is refused, or while the pointer comes from
/// the kernel's display queue: the old 2-tick poll.
const FALLBACK_TICKS: u64 = 2;

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
    // A desktop image installs its core apps at the console, before the
    // session takes the screen (`provisioning.rs`).
    #[cfg(lazyos_desktop)]
    provisioning::wait_for_core_packages();
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
    let mut screen = unsafe { Canvas::new(info.va, screen_w, screen_h) };
    // Compose in the framebuffer's byte order when it has one, so `present`
    // is a plain row copy (docs/performance-plan.md P3.2); RGBA otherwise.
    if sys::display_native_layout() == Ok(sys::screen_layout::BGRA)
        && sys::display_set_layout(sys::screen_layout::BGRA).is_ok()
    {
        screen.set_layout(display::PixelLayout::Bgra);
        sys::write_str("xuid: composing in BGRA (present is a row copy)\n");
    }
    let mut comp = Compositor::new(screen);
    probe::announce();
    // One receive buffer and one input batch for the whole life of the
    // compositor: the user bump allocator never reclaims, so the loop reuses
    // them instead of allocating per message.
    let mut request_buf = alloc::vec![0u8; messenger::DEFAULT_BUFFER];
    let mut input: Vec<Event> = Vec::with_capacity(MAX_INPUT);

    // First frame: the previous mux pixels are still on screen, so paint the
    // background and present before announcing readiness.
    comp.repaint_full();
    sys::write_str(UP_MARKER);
    sys::write_str(WM_MARKER);
    sys::write_str(SHELL_MARKER);
    // The self-tests probe fixed 1x pixel positions: run them at scale 1, then
    // put the desktop's scale back (no client has connected yet).
    let scale = theme::scale() as u32;
    theme::set_scale(1);
    for selftest in [
        keys::selftest_key_encoding,
        title::selftest_titles,
        window::selftest_focus_on_create,
        origin::selftest_open_origin,
        geometry::selftest_geometry,
        anim::selftest_anim,
        wheel::selftest_wheel_routing,
        drag::selftest_drag_target,
        reap::selftest_reap,
        pointer_feed::selftest_pointer_feed,
        held::selftest_held,
        cursor::selftest_cursor,
        shellcalls::selftest_shell_calls,
    ] {
        sys::write_str(selftest());
    }
    theme::set_scale(scale);

    loop {
        // Input an animation held comes before anything newer.
        comp.handle_held();

        // 0. `inputd`: register new surfaces, report focus, apply the
        //    sessions it opened (keys for those windows come from it).
        comp.sync_input();

        // 1. Input: drain the whole kernel queue first so pointer moves
        //    coalesce across poll calls, then handle what is left in order.
        //    Coalescing keeps a run of pointer moves at one entry, so a
        //    pointer moving as fast as the queue is drained cannot keep this
        //    loop from returning to serve requests (issue #339).
        held::drain_kernel(comp.input.owns_pointer, MAX_INPUT, |event| {
            push_coalesced(&mut input, event)
        });
        for event in input.drain(..) {
            comp.handle_event(event);
        }
        comp.handle_held();
        comp.reap_dead_shell();
        comp.tick_theme();
        comp.tick_power();
        comp.tick_opening();
        comp.tick_spinner();
        comp.reap_dead_surfaces(sys::clock());

        // 2. Park until a request, a shell event from `inputd` (pointer
        //    moves) or a key arrives, then serve one request if one came.
        let now = sys::clock();
        let mut sources = [server, server];
        let count = match comp.input_events() {
            Some(events) => {
                sources[1] = events;
                2
            }
            None => 1,
        };
        // No doorbell rings for a pointer move on the kernel's display queue
        // (the fallback when `inputd` does not own the pointer), so that
        // stream keeps the short poll.
        let idle = if comp.opening() {
            // The open zoom draws a frame per pass.
            1
        } else if comp.input.owns_pointer {
            IDLE_TICKS
        } else {
            FALLBACK_TICKS
        };
        let ready =
            match wait::wait_any(&sources[..count], wait::WAIT_DISPLAY_KEYS, Some(now + idle)) {
                Ok(mask) => mask,
                Err(error) if is_timeout(error) => 0,
                // Never spin on a refused wait: fall back to the timed receive.
                Err(_) => 1,
            };
        if ready & 1 == 0 {
            comp.reap_dead_shell();
            continue;
        }
        // Ready means queued, so this returns at once; the deadline only
        // bounds the fallback.
        let deadline = Some(now + FALLBACK_TICKS);
        match server.recv_with(&mut request_buf, deadline) {
            Ok(message) => {
                if let Some(txn) = message.txn {
                    let reply = comp.handle_request(&message);
                    let _ = server.reply(txn, &reply);
                } else if !protocol::carries_declared(&message) {
                    // A one-way message with undeclared transfers is dropped,
                    // and what it carried is closed rather than leaked.
                    protocol::drop_rejected_transfers(&message);
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

/// The most kernel records one drain reads before `xuid` handles them (the
/// kernel's own queue bound). The batch is allocated once, since the user
/// bump allocator never reclaims.
const MAX_INPUT: usize = held::CAPACITY;

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
