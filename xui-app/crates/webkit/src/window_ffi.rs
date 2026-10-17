//! The W5 window's half of the C ABI (feature `window`): `lzw_ui_main` runs the toolbar-and-page
//! window of `os.lazy.webkit`, and the calls that decide things answer for a window that asks for
//! nothing beyond the page: it opts out of the features (`lzw_ui_features` is 0, so the UI process
//! installs no download, dialog or hover handler), keeps no profile, and lets every navigation
//! load. LazyWebPlus (`xui-app/webplus`) defines these functions itself.

use std::ffi::{c_char, c_int};
use std::rc::Rc;
use std::sync::Arc;

use xui_core::app::run_app;
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::units::Dip;

use crate::abi::{first_address, Verdict};
use crate::app::{Browser, Host, APP_NAME};
use crate::ffi::{arguments, connect, FfiEngine};
use crate::mailbox;

/// The window size asked of the compositor: short enough that the status bar
/// clears the taskbar of a 720p desktop.
const WINDOW: (i32, i32) = (1000, 560);

/// What this window does beyond showing the page: nothing.
#[no_mangle]
pub extern "C" fn lzw_ui_features() -> c_int {
    0
}

/// The prefix of the UI process's markers.
#[no_mangle]
pub extern "C" fn lzw_ui_marker_prefix() -> *const c_char {
    c"WEBKIT:W5".as_ptr()
}

/// One marker line of the UI process, without the prefix (`webkit/port/app/log.cpp`):
/// `WEBKIT:W5:<line>`, one write on the trace channel. Any thread.
///
/// # Safety
/// `line` must be null or a NUL-terminated string.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_say(line: *const c_char) {
    // SAFETY: by this function's contract.
    let line = unsafe { crate::ffi::text(line) };
    xui_app::trace::line!(xui_app::trace::Channel::Webkit, "W5:{line}");
}

/// Where the engine keeps cookies and site data: nowhere (null), so nothing is persisted.
#[no_mangle]
pub extern "C" fn lzw_ui_profile_dir() -> *const c_char {
    std::ptr::null()
}

/// Every navigation loads.
#[no_mangle]
pub extern "C" fn lzw_ui_navigate(
    _url: *const c_char,
    _kind: c_int,
    _user_gesture: c_int,
    _committed_url: *const c_char,
) -> c_int {
    Verdict::Allow.code()
}

/// No content blocking (the UI process never asks: no feature), so no page is blocked.
#[no_mangle]
pub extern "C" fn lzw_ui_blocking(_url: *const c_char) -> c_int {
    0
}

/// Downloads are refused (the UI process never asks: no feature).
///
/// # Safety
/// The pointers are not used.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_download_start(
    _id: u64,
    _url: *const c_char,
    _suggested_name: *const c_char,
    _total: i64,
    _path: *mut c_char,
    _path_size: usize,
) -> c_int {
    1
}

/// The W5 window keeps no notification decisions (it does not ask for the feature).
///
/// # Safety
/// The pointers are not used.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_notify_permission_at(
    _index: usize,
    _origin: *mut c_char,
    _origin_size: usize,
) -> c_int {
    -1
}

/// Runs the window until it is closed. Returns the process exit status.
///
/// # Safety
/// `argv` must point at `argc` valid NUL-terminated strings (or be null).
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_main(argc: c_int, argv: *const *const c_char) -> c_int {
    // SAFETY: forwarded from this function's contract.
    let args = unsafe { arguments(argc, argv) };
    let first = first_address(&args);
    let Some(backend) = connect("WEBKIT:W5:UI") else {
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
        Browser::build(
            ui,
            Rc::new(FfiEngine),
            Arc::clone(mailbox::global()),
            first,
            host,
        )
        .expect("the WebKit window's widgets built")
    });
    backend.unbind();
    c_int::from(xui_app::launch::finish(
        xui_app::trace::Channel::Webkit,
        outcome,
    ))
}
