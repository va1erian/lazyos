//! `shellprobe`: the shell-protocol evidence client (issue #167, S5.0).
//!
//! It is a stand-in for LazyShell's display side: it subscribes to the
//! compositor's one-way shell events, creates the bottom-layer desktop surface,
//! reads back the window list / work area / chrome theme, and logs the
//! `SHELLPROBE:*:PASS` serial markers a scripted session greps for. The global
//! hotkeys themselves live in `xuid`; the probe observes their effects
//! (`StartMenu` on Ctrl+Esc/Super, `FocusChanged` when the Alt+Tab selection
//! commits).
//!
//! Boot it with `LAZYOS_XUID=1` plus the `LAZYOS_SHELLPROBE=1` demo hook; the
//! kernel then starts `XUID.ELF` and this program (`SHELLPRB.ELF`). Without the
//! hook the default `xuid` + `xdemo` + drag & drop sessions are untouched.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;
use user::messenger;
use user::messenger::display::{self, Canvas, Client, Color, Rect, ShellEvent};
use user::sys;

/// The probe's own window content size in pixels.
const WINDOW_W: i32 = 320;
const WINDOW_H: i32 = 200;

/// Serial markers the evidence session greps for.
const DESKTOP_MARKER: &str = "SHELLPROBE:DESKTOP:PASS\n";
const LIST_MARKER: &str = "SHELLPROBE:LIST:PASS\n";
const FOCUS_MARKER: &str = "SHELLPROBE:FOCUS:PASS\n";
const HOTKEY_MARKER: &str = "SHELLPROBE:HOTKEY:PASS\n";

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("shellprobe: shell-protocol evidence client starting (issue #167)\n");
    run()
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    sys::write_str("shellprobe: panic\n");
    let _ = info;
    sys::exit(1)
}

/// Report a fatal startup failure on serial, then exit.
fn fail(what: &str, code: i64) -> ! {
    sys::write_str("shellprobe: fatal: ");
    sys::write_str(what);
    sys::write_str(": ");
    sys::write_str(&alloc::format!("{code}\n"));
    sys::exit(1)
}

/// Whether an error is a `recv` deadline firing.
fn is_timeout(error: messenger::Error) -> bool {
    matches!(error, messenger::Error::Errno(code) if code == -messenger::errno::ETIMEDOUT)
}

/// Connect as an `os.lazy.display.v1` client, register as the shell, create the
/// desktop and a probe window, verify the protocol read-backs, then observe the
/// shell events the compositor sends.
fn run() -> ! {
    let client = match Client::connect() {
        Ok(client) => client,
        Err(error) => fail("connect", error.errno().unwrap_or(0)),
    };

    // Subscribe first: registering the `"shell"` role hides xuid's fallback
    // taskbar, which expands the work area to the whole screen.
    let (shell_events, shell_published) = match messenger::create_pair() {
        Ok(pair) => pair,
        Err(error) => fail("create_pair", error.errno().unwrap_or(0)),
    };
    if let Err(error) = client.subscribe(display::ROLE_SHELL, &shell_published) {
        fail("subscribe", error.errno().unwrap_or(0));
    }

    let work = match client.get_work_area() {
        Ok(work) => work,
        Err(error) => fail("get_work_area", error.errno().unwrap_or(0)),
    };
    if work.w <= 0 || work.h <= 0 {
        fail("work area is empty", work.w as i64);
    }
    match client.get_theme() {
        Ok(theme) => {
            let hex = |color: Color| -> u32 {
                ((color.r as u32) << 16) | ((color.g as u32) << 8) | color.b as u32
            };
            sys::write_str(&alloc::format!(
                "SHELLPROBE:THEME:{:06x}:{:06x}:{:06x}:{:06x}:{:06x}\n",
                hex(theme.title_bg_active),
                hex(theme.title_bg_inactive),
                hex(theme.border),
                hex(theme.taskbar),
                hex(theme.text),
            ));
        }
        Err(error) => fail("get_theme", error.errno().unwrap_or(0)),
    }

    // The bottom-layer desktop, painted across the work area. The desktop's
    // event endpoint is transferred for protocol symmetry; xuid never focuses
    // or hit-tests a desktop, so nothing arrives on it.
    let (_desktop_events, desktop_published) = match messenger::create_pair() {
        Ok(pair) => pair,
        Err(error) => fail("create_pair", error.errno().unwrap_or(0)),
    };
    let first_desktop = match client.create_desktop_surface(
        work.w as u64,
        work.h as u64,
        "desktop",
        &desktop_published,
    ) {
        Ok(desktop) => desktop,
        Err(error) => fail("create_desktop_surface", error.errno().unwrap_or(0)),
    };
    let desktop_bytes = (work.w * work.h * 4) as u64;
    let (desktop_buffer, desktop_va) = match sys::display_create_buffer(desktop_bytes) {
        Ok(buffer) => buffer,
        Err(code) => fail("create_buffer(desktop)", code),
    };
    // Safety: `desktop_va` maps the buffer just created, `work.w * work.h * 4`
    // bytes long.
    let mut desktop_canvas = unsafe { Canvas::new(desktop_va, work.w, work.h) };
    paint_desktop(&mut desktop_canvas, work);
    if let Err(error) = client.attach_buffer(first_desktop, desktop_buffer, desktop_bytes) {
        fail("attach_buffer(desktop)", error.errno().unwrap_or(0));
    }
    if let Err(error) = client.commit(first_desktop, work) {
        fail("commit(desktop)", error.errno().unwrap_or(0));
    }

    // Replacing an existing desktop is allowed: a second desktop takes the
    // bottom layer and the first one is gone (the list check proves it).
    let (_replacement_events, replacement_published) = match messenger::create_pair() {
        Ok(pair) => pair,
        Err(error) => fail("create_pair", error.errno().unwrap_or(0)),
    };
    let desktop = match client.create_desktop_surface(
        work.w as u64,
        work.h as u64,
        "desktop",
        &replacement_published,
    ) {
        Ok(desktop) => desktop,
        Err(error) => fail(
            "create_desktop_surface(replacement)",
            error.errno().unwrap_or(0),
        ),
    };
    if let Err(error) = client.attach_buffer(desktop, desktop_buffer, desktop_bytes) {
        fail("attach_buffer(replacement)", error.errno().unwrap_or(0));
    }
    if let Err(error) = client.commit(desktop, work) {
        fail("commit(replacement)", error.errno().unwrap_or(0));
    }
    sys::write_str(DESKTOP_MARKER);

    // One probe window, so the list has a second row and the Alt+Tab cycle has
    // a target the compositor can focus.
    let (window_events, window_published) = match messenger::create_pair() {
        Ok(pair) => pair,
        Err(error) => fail("create_pair", error.errno().unwrap_or(0)),
    };
    let window = match client.create_surface(
        WINDOW_W as u64,
        WINDOW_H as u64,
        "shellprobe",
        &window_published,
    ) {
        Ok(window) => window,
        Err(error) => fail("create_surface", error.errno().unwrap_or(0)),
    };
    let window_bytes = (WINDOW_W * WINDOW_H * 4) as u64;
    let (window_buffer, window_va) = match sys::display_create_buffer(window_bytes) {
        Ok(buffer) => buffer,
        Err(code) => fail("create_buffer(window)", code),
    };
    // Safety: `window_va` maps the buffer just created, `WINDOW_W * WINDOW_H *
    // 4` bytes long.
    let mut window_canvas = unsafe { Canvas::new(window_va, WINDOW_W, WINDOW_H) };
    paint_window(&mut window_canvas, work);
    if let Err(error) = client.attach_buffer(window, window_buffer, window_bytes) {
        fail("attach_buffer(window)", error.errno().unwrap_or(0));
    }
    if let Err(error) = client.commit(window, Rect::new(0, 0, WINDOW_W, WINDOW_H)) {
        fail("commit(window)", error.errno().unwrap_or(0));
    }

    // Read back the surface table: the desktop and the window must both be
    // there with their geometry, and exactly one row is focused.
    match client.list_surfaces() {
        Ok(rows) => {
            let desktop_row = rows
                .iter()
                .find(|row| row.id == desktop)
                .filter(|row| row.title == "desktop" && row.w == work.w && row.h == work.h);
            let window_row = rows
                .iter()
                .find(|row| row.id == window)
                .filter(|row| row.title == "shellprobe" && row.w == WINDOW_W && row.h == WINDOW_H);
            let focused = rows.iter().filter(|row| row.focused).count();
            let replaced = rows.iter().all(|row| row.id != first_desktop);
            if desktop_row.is_some() && window_row.is_some() && focused == 1 && replaced {
                sys::write_str(&alloc::format!(
                    "shellprobe: window list has {} rows, {} focused\n",
                    rows.len(),
                    focused
                ));
                sys::write_str(LIST_MARKER);
            } else {
                sys::write_str(&alloc::format!(
                    "SHELLPROBE:LIST:FAIL:rows={} focused={} replaced={} desktop={:?} window={:?}\n",
                    rows.len(),
                    focused,
                    replaced,
                    desktop_row.map(|row| (row.x, row.y, row.w, row.h)),
                    window_row.map(|row| (row.x, row.y, row.w, row.h)),
                ));
            }
        }
        Err(error) => {
            sys::write_str(&alloc::format!(
                "SHELLPROBE:LIST:FAIL:{}\n",
                error.errno().unwrap_or(0)
            ));
        }
    }

    // A borrowed lifetime is not available on the endpoints (`create_pair`
    // hands out owned values) and the compositor never closes its end, so the
    // shell channel is the blocking one and the window channel is polled.
    let mut shell_buf = alloc::vec![0u8; 4096];
    let mut window_buf = alloc::vec![0u8; 4096];
    let mut focus_seen = false;
    let mut hotkey_seen = false;
    loop {
        let deadline = Some(sys::clock() + 1);
        match shell_events.recv_with(&mut shell_buf, deadline) {
            Ok(message) => match display::decode_shell_event(&message) {
                Some(ShellEvent::FocusChanged(id)) if !focus_seen && id.is_some() => {
                    focus_seen = true;
                    sys::write_str(FOCUS_MARKER);
                }
                Some(ShellEvent::StartMenu) if !hotkey_seen => {
                    hotkey_seen = true;
                    sys::write_str(HOTKEY_MARKER);
                }
                _ => {}
            },
            Err(error) if is_timeout(error) => {}
            Err(messenger::Error::Errno(code)) if code == -messenger::errno::EPIPE => {
                // The compositor died; there is nothing left to observe.
                sys::exit(0)
            }
            Err(_) => {}
        }

        // The window manager's close button (or Alt+F4) asks the probe's
        // window to go away; take the desktop down with it and exit.
        if let Ok(Some(message)) = window_events.poll_recv_with(&mut window_buf) {
            if message.method() == display::method::WINDOW_CLOSE {
                let _ = client.destroy_surface(window);
                let _ = client.destroy_surface(desktop);
                sys::write_str("shellprobe: closed by the window manager\n");
                sys::exit(0);
            }
        }
    }
}

/// Paint the desktop wallpaper: a teal backdrop with a dotted grid and a
/// label, so the bottom layer is unmistakable in a screenshot.
fn paint_desktop(canvas: &mut Canvas, work: Rect) {
    let base = Color::rgb(0, 96, 110);
    let dot = Color::rgb(12, 44, 54);
    canvas.fill(work, work, base);
    let mut y = 8;
    while y < work.h {
        let mut x = 8;
        while x < work.w {
            canvas.fill(Rect::new(x, y, 2, 2), work, dot);
            x += 32;
        }
        y += 32;
    }
    canvas.text(24, 20, "lazyos desktop", Color::rgb(215, 242, 246), work, 2);
    canvas.text(
        24,
        44,
        "shellprobe (issue #167)",
        Color::rgb(150, 210, 220),
        work,
        1,
    );
}

/// Paint the probe window's content: a light panel with a title strip and the
/// work-area geometry the shell read back.
fn paint_window(canvas: &mut Canvas, work: Rect) {
    let clip = Rect::new(0, 0, WINDOW_W, WINDOW_H);
    canvas.fill(clip, clip, Color::rgb(236, 236, 236));
    canvas.fill(Rect::new(0, 0, WINDOW_W, 22), clip, Color::rgb(30, 36, 54));
    canvas.text(8, 7, "shellprobe", Color::rgb(240, 244, 255), clip, 1);
    canvas.text(
        10,
        40,
        "display client of xuid",
        Color::rgb(24, 24, 32),
        clip,
        1,
    );
    canvas.text(
        10,
        58,
        &alloc::format!("work area {}x{}", work.w, work.h),
        Color::rgb(24, 24, 32),
        clip,
        1,
    );
    canvas.text(
        10,
        76,
        "desktop + events live",
        Color::rgb(24, 24, 32),
        clip,
        1,
    );
}
