//! The C ABI between WebKit's UI process (C++, `webkit/port/app/`) and this
//! window. `webkit/port/app/lazyos_ui.h` is the other half of every function
//! here.
//!
//! The process has two threads that matter. WebKit's run loop owns the
//! process's main thread (as in `webkit-smoke`); the C++ `main` starts a second
//! thread that calls [`lzw_ui_main`], which runs the window until it closes.
//! Everything the window asks of WebKit (`lzw_engine_*`, implemented in C++)
//! only queues work for the run loop; everything WebKit says
//! (`lzw_ui_frame`, `lzw_ui_state`) goes through the [`Mailbox`].
//!
//! This is a static library with no `main`: Rust's runtime start-up never ran,
//! so `std::env::args` is empty. The arguments come in explicitly, and the
//! window is always a compositor client.

use std::ffi::{c_char, c_int, CStr, CString};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use xui_app::backend::LazyOSBackend;
use xui_core::app::run_app;
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;

use crate::address;
use crate::app::{Browser, Host, APP_NAME};
use crate::engine::{Button, Engine, MouseKind};
use crate::mailbox::{Mailbox, PageState};

/// The window size asked of the compositor: short enough that the status bar
/// clears the taskbar of a 720p desktop.
const WINDOW: (i32, i32) = (1000, 560);

/// How long to wait for the compositor before giving up.
const CONNECT_WAIT: Duration = Duration::from_secs(900);

extern "C" {
    fn lzw_engine_load(url: *const c_char);
    fn lzw_engine_reload();
    fn lzw_engine_stop();
    fn lzw_engine_back();
    fn lzw_engine_forward();
    fn lzw_engine_resize(width: c_int, height: c_int);
    fn lzw_engine_mouse(kind: c_int, button: c_int, x: c_int, y: c_int, mods: u32);
    fn lzw_engine_wheel(x: c_int, y: c_int, ticks_x: c_int, ticks_y: c_int, mods: u32);
    fn lzw_engine_key(press: c_int, keysym: u32, text: *const c_char, mods: u32);
    fn lzw_engine_focus(focused: c_int);
}

fn mailbox() -> &'static Arc<Mailbox> {
    static MAILBOX: OnceLock<Arc<Mailbox>> = OnceLock::new();
    MAILBOX.get_or_init(|| Arc::new(Mailbox::default()))
}

/// A C string from text that may not be one (a NUL ends it).
fn c_string(text: &str) -> CString {
    let end = text.find('\0').unwrap_or(text.len());
    CString::new(&text[..end]).unwrap_or_default()
}

/// The engine behind the C++ functions.
struct FfiEngine;

impl Engine for FfiEngine {
    fn load(&self, url: &str) {
        let url = c_string(url);
        // SAFETY: `url` is a NUL-terminated string that outlives the call; the
        // C++ side copies it before returning.
        unsafe { lzw_engine_load(url.as_ptr()) }
    }
    fn reload(&self) {
        // SAFETY: no arguments.
        unsafe { lzw_engine_reload() }
    }
    fn stop(&self) {
        // SAFETY: no arguments.
        unsafe { lzw_engine_stop() }
    }
    fn back(&self) {
        // SAFETY: no arguments.
        unsafe { lzw_engine_back() }
    }
    fn forward(&self) {
        // SAFETY: no arguments.
        unsafe { lzw_engine_forward() }
    }
    fn resize(&self, width: i32, height: i32) {
        // SAFETY: plain integers.
        unsafe { lzw_engine_resize(width, height) }
    }
    fn mouse(&self, kind: MouseKind, button: Button, x: i32, y: i32, mods: u32) {
        // SAFETY: plain integers.
        unsafe { lzw_engine_mouse(kind.code(), button.code(), x, y, mods) }
    }
    fn wheel(&self, x: i32, y: i32, ticks_x: i32, ticks_y: i32, mods: u32) {
        // SAFETY: plain integers.
        unsafe { lzw_engine_wheel(x, y, ticks_x, ticks_y, mods) }
    }
    fn key(&self, press: bool, keysym: u32, text: &str, mods: u32) {
        let text = c_string(text);
        // SAFETY: `text` is a NUL-terminated string that outlives the call.
        unsafe { lzw_engine_key(c_int::from(press), keysym, text.as_ptr(), mods) }
    }
    fn focus(&self, focused: bool) {
        // SAFETY: a plain integer.
        unsafe { lzw_engine_focus(c_int::from(focused)) }
    }
}

/// The arguments the C++ `main` got, as text.
///
/// # Safety
/// `argv` must point at `argc` valid NUL-terminated strings (or be null).
unsafe fn arguments(argc: c_int, argv: *const *const c_char) -> Vec<String> {
    if argv.is_null() {
        return Vec::new();
    }
    (0..usize::try_from(argc).unwrap_or(0))
        .filter_map(|at| {
            // SAFETY: `at < argc`, and the caller vouches for the array.
            let pointer = unsafe { *argv.add(at) };
            // SAFETY: a non-null entry is a NUL-terminated string.
            (!pointer.is_null()).then(|| {
                unsafe { CStr::from_ptr(pointer) }
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect()
}

/// Whether this image carries the UI probe: the C++ side prints the markers that name pages
/// (and this side `UI:NAV`) only then.
#[no_mangle]
pub extern "C" fn lzw_ui_probe_enabled() -> c_int {
    c_int::from(xui_app::probe::enabled())
}

/// The calling user's user-memory charge and limit, in bytes, as the kernel counts them: the
/// length of every mapping, not the pages touched, so what a JIT or a collector reserves counts
/// (the page's WebProcess, the NetworkProcess and this process share one user's quota).
/// Returns 0 on success.
///
/// # Safety
/// `used` and `limit` must be valid for a write.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_user_memory(used: *mut u64, limit: *mut u64) -> c_int {
    let Ok(charge) = lazyos_sys::quota::user_memory() else {
        return -1;
    };
    if used.is_null() || limit.is_null() {
        return -1;
    }
    // SAFETY: the caller promises both pointers are writable.
    unsafe {
        *used = charge.used;
        *limit = charge.limit;
    }
    0
}

/// The compositor client, waiting for the compositor: an app that opens at login starts while the
/// core apps are still being installed, and `xuid` serves no display until they are (LazyShell
/// retries the same way, `SHELL:CONNECT:RETRY`).
fn connect() -> Option<Rc<LazyOSBackend>> {
    let started = Instant::now();
    let mut said = false;
    loop {
        match LazyOSBackend::new_client() {
            Ok(backend) => return Some(Rc::new(backend)),
            Err(code) if started.elapsed() < CONNECT_WAIT => {
                if !said {
                    println!("WEBKIT:W5:UI:WAIT:{code}");
                    said = true;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(code) => {
                println!("WEBKIT:W5:UI:BIND:FAIL:{code}");
                return None;
            }
        }
    }
}

/// Runs the window until it is closed. Returns the process exit status.
///
/// # Safety
/// `argv` must point at `argc` valid NUL-terminated strings (or be null).
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_main(argc: c_int, argv: *const *const c_char) -> c_int {
    // SAFETY: forwarded from this function's contract.
    let args = unsafe { arguments(argc, argv) };
    let first = args
        .iter()
        .skip(1)
        .find(|arg| !arg.starts_with('-') && !arg.starts_with("attempt="))
        .and_then(|arg| address::normalize(arg));
    let Some(backend) = connect() else {
        return 1;
    };
    let (width, height) = backend.window_size(WINDOW);
    // Below this the toolbar clips.
    backend.set_size_hints(360, 240, 0, 0);
    backend.on_first_frame(|| println!("WEBKIT:W5:UI:UP"));
    let theme = backend.desktop_theme();
    let spec = PlatformSpec::new(APP_NAME).size(Dip(width as f32), Dip(height as f32));
    let focus_backend = Rc::clone(&backend);
    let outcome = run_app(Rc::clone(&backend) as Rc<dyn Backend>, spec, move |ui| {
        if let Some(theme) = theme {
            ui.set_theme(theme);
        }
        let host = Host {
            traces: xui_app::probe::enabled(),
            focused: Box::new(move || focus_backend.focused()),
            probe: Box::new(|window, name, rect| {
                xui_app::probe::widget(
                    window,
                    name,
                    rect.left,
                    rect.top,
                    rect.width(),
                    rect.height(),
                );
            }),
        };
        Browser::build(ui, Rc::new(FfiEngine), Arc::clone(mailbox()), first, host)
            .expect("the WebKit window's widgets built")
    });
    backend.unbind();
    c_int::from(xui_app::launch::finish("WEBKIT:W5:UI", outcome))
}

/// WebKit painted a frame: `rgba` is `width * height * 4` bytes, top-down,
/// opaque.
///
/// # Safety
/// `rgba` must point at that many readable bytes.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_frame(rgba: *const u8, width: c_int, height: c_int) {
    let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
        return;
    };
    let Some(len) = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
    else {
        return;
    };
    if rgba.is_null() || len == 0 {
        return;
    }
    // SAFETY: the caller promises `len` readable bytes.
    let pixels = unsafe { std::slice::from_raw_parts(rgba, len) };
    mailbox().post_frame(width, height, pixels);
}

/// The page's state changed.
///
/// # Safety
/// `url` and `title` must be null or NUL-terminated strings.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_state(
    url: *const c_char,
    title: *const c_char,
    loading: c_int,
    can_back: c_int,
    can_forward: c_int,
    progress: c_int,
) {
    let text = |pointer: *const c_char| {
        if pointer.is_null() {
            String::new()
        } else {
            // SAFETY: a non-null argument is a NUL-terminated string.
            unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned()
        }
    };
    mailbox().post_state(PageState {
        url: text(url),
        title: text(title),
        loading: loading != 0,
        can_back: can_back != 0,
        can_forward: can_forward != 0,
        progress: u32::try_from(progress).unwrap_or(0).min(1000),
    });
}
