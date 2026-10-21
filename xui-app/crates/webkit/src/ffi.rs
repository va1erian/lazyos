//! The C ABI between WebKit's UI process (C++, `webkit/port/app/`) and the window, the half every
//! window shares. `webkit/port/app/lazyos_ui.h` is the other half of every function here.
//!
//! The process has two threads that matter. WebKit's run loop owns the process's main thread (as
//! in `webkit-smoke`); the C++ `main` starts a second thread that calls `lzw_ui_main`, which runs
//! the window until it closes. Everything the window asks of WebKit (`lzw_engine_*`, implemented in
//! C++) only queues work for the run loop; everything WebKit says (`lzw_ui_frame`, `lzw_ui_state`,
//! `lzw_ui_hover`, ...) goes through the [`Mailbox`](crate::mailbox::Mailbox).
//!
//! The functions that differ from one window to another (`lzw_ui_main`, `lzw_ui_features`,
//! `lzw_ui_profile_dir`, `lzw_ui_navigate`, `lzw_ui_download_start`, `lzw_ui_marker_prefix`) are
//! defined by the window's own crate, `xui-app/webplus`.
//!
//! This is a static library with no `main`: Rust's runtime start-up never ran, so
//! `std::env::args` is empty. The arguments come in explicitly, and the window is always a
//! compositor client.

use std::ffi::{c_char, c_int, CStr};
use std::rc::Rc;
use std::time::{Duration, Instant};

use xui_app::backend::LazyOSBackend;

use crate::abi::{
    byte_count, frame_geometry, notify_permission, notify_show, page_state, ListStatus,
};
use crate::engine::DialogKind;
use crate::events::{CursorKind, Event};
use crate::mailbox;

/// How long to wait for the compositor before giving up.
const CONNECT_WAIT: Duration = Duration::from_secs(900);

mod engine;
pub use engine::FfiEngine;

/// The arguments the C++ `main` got, as text.
///
/// # Safety
/// `argv` must point at `argc` valid NUL-terminated strings (or be null).
pub unsafe fn arguments(argc: c_int, argv: *const *const c_char) -> Vec<String> {
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

/// `text` for a NUL-terminated argument that may be null.
///
/// # Safety
/// `pointer` must be null or a NUL-terminated string.
pub unsafe fn text(pointer: *const c_char) -> String {
    if pointer.is_null() {
        String::new()
    } else {
        // SAFETY: a non-null argument is a NUL-terminated string.
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned()
    }
}

/// Whether this image carries the UI probe: the C++ side prints the markers that name pages
/// (and the window the ones that name addresses) only then.
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
/// retries the same way, `SHELL:CONNECT:RETRY`). `marker` is the window's marker prefix.
pub fn connect(marker: &str) -> Option<Rc<LazyOSBackend>> {
    let started = Instant::now();
    let mut said = false;
    loop {
        match LazyOSBackend::new_client() {
            Ok(backend) => return Some(Rc::new(backend)),
            Err(code) if started.elapsed() < CONNECT_WAIT => {
                if !said {
                    println!("{marker}:WAIT:{code}");
                    said = true;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(code) => {
                println!("{marker}:BIND:FAIL:{code}");
                return None;
            }
        }
    }
}

/// WebKit painted a frame: `rgba` is exactly `width * height * 4` bytes, top row first, opaque.
/// The bytes are copied into the mailbox before this returns, so the pointer needs to be valid
/// for the call only (the engine reuses its buffer for the next frame).
///
/// # Safety
/// `rgba` must point at `width * height * 4` readable bytes for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_frame(rgba: *const u8, width: c_int, height: c_int) {
    let Some((width, height, len)) = frame_geometry(width, height) else {
        return;
    };
    if rgba.is_null() {
        return;
    }
    // SAFETY: the caller promises `len` readable bytes.
    let pixels = unsafe { std::slice::from_raw_parts(rgba, len) };
    mailbox::global().post_frame(width, height, pixels);
}

/// The page's state changed.
///
/// # Safety
/// `url`, `title`, `error` and `committed_url` must be null or NUL-terminated strings.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_state(
    url: *const c_char,
    title: *const c_char,
    loading: c_int,
    can_back: c_int,
    can_forward: c_int,
    progress: c_int,
    error: *const c_char,
    crashed: c_int,
    committed_url: *const c_char,
    secure: c_int,
    finished: c_int,
) {
    // SAFETY: each argument is null or a NUL-terminated string, by this function's contract.
    let state = unsafe {
        page_state(
            text(url),
            text(title),
            loading,
            can_back,
            can_forward,
            progress,
            text(error),
            crashed,
            text(committed_url),
            secure,
            finished,
        )
    };
    mailbox::global().post_state(state);
}

/// The page's WebProcess stopped answering WebKit's responsiveness pings (`responsive` 0), answers
/// again (1), or is gone (1: nothing is left to wait for).
#[no_mangle]
pub extern "C" fn lzw_ui_page_responsive(responsive: c_int) {
    mailbox::global().post_event(Event::PageResponsive(responsive != 0));
}

/// The pointer is over a link (`url`), or no longer (empty).
///
/// # Safety
/// `url` must be null or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_hover(url: *const c_char) {
    // SAFETY: by this function's contract.
    let url = unsafe { text(url) };
    mailbox::global().post_event(Event::Hover(url));
}

/// The page wants this pointer shape over itself (`CursorKind::code`).
#[no_mangle]
pub extern "C" fn lzw_ui_cursor(kind: c_int) {
    mailbox::global().post_event(Event::Cursor(CursorKind::from_code(kind)));
}

/// A page asked for a new window (`target=_blank`, `window.open`). Only a request with a user
/// gesture is passed on: the window opens `url` in its one view.
///
/// # Safety
/// `url` must be null or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_new_window(url: *const c_char, user_gesture: c_int) {
    if user_gesture == 0 {
        return;
    }
    // SAFETY: by this function's contract.
    let url = unsafe { text(url) };
    mailbox::global().post_event(Event::Popup { url });
}

/// Download `id` received `received` bytes of `total` (negative: not announced).
#[no_mangle]
pub extern "C" fn lzw_ui_download_progress(id: u64, received: i64, total: i64) {
    mailbox::global().post_event(Event::DownloadProgress {
        id,
        received: byte_count(received).unwrap_or(0),
        total: byte_count(total),
    });
}

/// Download `id` completed: every byte is in the file.
#[no_mangle]
pub extern "C" fn lzw_ui_download_done(id: u64) {
    mailbox::global().post_event(Event::DownloadDone { id });
}

/// Download `id` failed, or was cancelled; `reason` says why.
///
/// # Safety
/// `reason` must be null or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_download_fail(id: u64, reason: *const c_char) {
    // SAFETY: by this function's contract.
    let reason = unsafe { text(reason) };
    mailbox::global().post_event(Event::DownloadFailed { id, reason });
}

/// Rule list `id` became ready, or did not (`ListStatus::code`).
///
/// # Safety
/// `id` must be null or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_blocking_list(id: *const c_char, status: c_int) {
    // SAFETY: by this function's contract.
    let id = unsafe { text(id) };
    mailbox::global().post_event(Event::BlockingList {
        id,
        status: ListStatus::from_code(status),
    });
}

/// The number of loads the rule lists blocked on the page on show.
#[no_mangle]
pub extern "C" fn lzw_ui_blocked(count: c_int) {
    mailbox::global().post_event(Event::Blocked(u32::try_from(count).unwrap_or(0)));
}

/// A page opened a JavaScript dialog; the answer comes back through `lzw_engine_dialog_reply`.
/// Returns at once: the window draws the dialog on its own thread.
///
/// # Safety
/// `message` and `default_value` must be null or NUL-terminated strings.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_dialog(
    id: u64,
    kind: c_int,
    message: *const c_char,
    default_value: *const c_char,
) {
    // SAFETY: by this function's contract.
    let (text, default) = unsafe { (text(message), text(default_value)) };
    mailbox::global().post_event(Event::Dialog {
        id,
        kind: DialogKind::from_code(kind),
        text,
        default,
    });
}

/// A page asked to show a notification (WebKit's provider `show`). Every text is the page's own and
/// untrusted: the window trims and cleans it before it reaches the shell. Returns at once.
///
/// # Safety
/// `origin`, `page_url`, `title`, `body` and `tag` must be null or NUL-terminated strings.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_notify_show(
    id: u64,
    origin: *const c_char,
    page_url: *const c_char,
    title: *const c_char,
    body: *const c_char,
    tag: *const c_char,
) {
    // SAFETY: by this function's contract.
    let event = unsafe {
        notify_show(
            id,
            text(origin),
            text(page_url),
            text(title),
            text(body),
            text(tag),
        )
    };
    mailbox::global().post_event(event);
}

/// The page took notification `id` down itself (`Notification.close()`).
#[no_mangle]
pub extern "C" fn lzw_ui_notify_cancel(id: u64) {
    mailbox::global().post_event(Event::NotifyCancel { id });
}

/// The page's object for notification `id` is gone (its document was): the toast stays.
#[no_mangle]
pub extern "C" fn lzw_ui_notify_gone(id: u64) {
    mailbox::global().post_event(Event::NotifyGone { id });
}

/// The engine refused the pending permission requests of `origin` itself (their page went away).
///
/// # Safety
/// `origin` must be null or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_notify_permission_gone(origin: *const c_char) {
    // SAFETY: by this function's contract.
    let origin = unsafe { text(origin) };
    mailbox::global().post_event(Event::NotifyPermissionGone { origin });
}

/// A page asked for the permission to notify (`Notification.requestPermission()`); the answer is
/// `lzw_engine_notify_permission(origin, allow)`.
///
/// # Safety
/// `origin` must be null or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_notify_permission_request(
    origin: *const c_char,
    user_gesture: c_int,
) {
    // SAFETY: by this function's contract.
    let origin = unsafe { text(origin) };
    mailbox::global().post_event(notify_permission(origin, user_gesture));
}
