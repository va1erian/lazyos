//! The C functions `webkit/port/webd/main.cpp` calls (`webkit/port/webd/host.h`).
//!
//! `main.cpp` runs WebKit on a thread of its own and the service on the process's first thread, in
//! this order: `webd_host_init` (the doorbell, the engine's event queue and its installation as
//! the sink of the `webd_ui_*` callbacks), then the WebKit thread starts the engine, then
//! `webd_host_main` serves until it is told to stop, then `webd_host_report` says how the shutdown
//! went. `webd_host_signal_quit` is the SIGTERM handler's way in.

use std::ffi::{c_char, c_int, CStr};
use std::sync::{Arc, OnceLock};

use xui_app::trace::{self, Channel};
use xui_webd::events::EventQueue;
use xui_webd::ffi;

use crate::{doorbell, serve};

/// The queue the engine's callbacks fill and the service drains.
static QUEUE: OnceLock<Arc<EventQueue>> = OnceLock::new();

/// The calling user's memory charge and limit, for the engine's process-creation gate.
fn user_memory() -> Option<(u64, u64)> {
    let charge = lazyos_sys::quota::user_memory().ok()?;
    Some((charge.used, charge.limit))
}

/// The package's `system_name`: the engine's data lives in `$HOME/.apps/os.lazy.webd`.
const SYSTEM_NAME: &str = "os.lazy.webd";

/// Point the engine at a folder for its network-lock rule store (`LAZYOS_WEBD_RULES_DIR`,
/// `webkit/port/app/light.cpp`): the one thing a light view makes it write. Inside the app's own
/// data folder, which the manifest lets the app write. The folder is required and there is no
/// fallback (not a shared one like `/tmp`): without `$HOME`, or when the folder cannot be made, the
/// variable stays unset and the engine refuses to prepare the network lock, so no light view is
/// served. Before the engine starts, while the process has one thread.
fn choose_rules_folder() {
    let Some(home) = std::env::var("HOME")
        .ok()
        .filter(|home| home.starts_with('/'))
    else {
        return;
    };
    let folder = format!("{}/rules", fhs::app_data_dir(&home, SYSTEM_NAME));
    if std::fs::create_dir_all(&folder).is_ok() {
        std::env::set_var("LAZYOS_WEBD_RULES_DIR", folder);
    }
}

/// Make the doorbell and the event queue and install the queue as the engine's sink. Once, before
/// `webd_engine_start`. Returns 0, or non-zero when the doorbell cannot be made.
#[no_mangle]
pub extern "C" fn webd_host_init() -> c_int {
    choose_rules_folder();
    if let Err(error) = doorbell::create() {
        trace::fail!(Channel::Webd, "UP", "doorbell: {error}");
        return 1;
    }
    let queue = Arc::new(
        EventQueue::new()
            .with_wake(doorbell::ring)
            .with_memory(user_memory),
    );
    ffi::install(queue.clone());
    // A second call would replace nothing: the first queue stays the service's.
    let _ = QUEUE.set(queue);
    0
}

/// Serve `app.os.lazy.webd.light` until `Quit`, SIGTERM or a hang. Blocks; on the process's first
/// thread. Returns the exit status (0, 1 not serving, 4 the engine hung).
#[no_mangle]
pub extern "C" fn webd_host_main() -> c_int {
    match QUEUE.get() {
        Some(queue) => serve::run(queue.clone()),
        None => serve::EXIT_NOT_SERVING,
    }
}

/// Ask `webd_host_main` to return. Any thread, and a signal handler.
#[no_mangle]
pub extern "C" fn webd_host_signal_quit() {
    serve::request_quit();
}

/// The engine could not start: say so (`why` is a NUL-terminated reason).
///
/// # Safety
/// `why` is NULL or a NUL-terminated string valid for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_host_fail(why: *const c_char) {
    let why = if why.is_null() {
        String::from("unknown")
    } else {
        // SAFETY: by the contract above.
        unsafe { CStr::from_ptr(why) }
            .to_string_lossy()
            .into_owned()
    };
    trace::fail!(Channel::Webd, "UP", "{why}");
}

/// The last word: the service's exit `status` and how many engine processes `surviving` the
/// shutdown could not end. Prints `WEBD:QUIT:PASS` or `WEBD:QUIT:FAIL`.
#[no_mangle]
pub extern "C" fn webd_host_report(status: c_int, surviving: c_int) {
    if status == serve::EXIT_CLEAN && surviving == 0 {
        trace::pass!(Channel::Webd, "QUIT");
    } else {
        trace::fail!(
            Channel::Webd,
            "QUIT",
            "status={status} engine processes left={surviving}"
        );
    }
}
