//! The `webd_engine_*` calls of full views (`webd_abi.h`, "P2 slice 3"), and the host's start with
//! the children's environment decided once.

use std::ffi::{c_char, c_int, CString};
use std::path::Path;

use super::c_string;

#[repr(C)]
struct WebdHostConfig {
    struct_size: u32,
    children_directory: *const c_char,
}

extern "C" {
    fn webd_engine_start_host(config: *const WebdHostConfig) -> c_int;
    fn webd_engine_load(view: u64, url: *const c_char);
    fn webd_engine_reload(view: u64);
    fn webd_engine_stop(view: u64);
    fn webd_engine_back(view: u64);
    fn webd_engine_forward(view: u64);
    fn webd_engine_download(view: u64, url: *const c_char);
    fn webd_engine_download_cancel(view: u64, download: u64);
    fn webd_engine_dialog_reply(view: u64, dialog: u64, accepted: c_int, value: *const c_char);
    fn webd_engine_navigation_answer(view: u64, request: u64, allow: c_int);
    fn webd_engine_notify_shown(id: u64);
    fn webd_engine_notify_clicked(id: u64);
    fn webd_engine_notify_closed(id: u64);
    fn webd_engine_notify_permission(view: u64, origin: *const c_char, answer: c_int);
    fn webd_engine_blocking_load(view: u64, list: *const c_char);
    fn webd_engine_blocking_compile(
        view: u64,
        list: *const c_char,
        json: *const c_char,
        len: usize,
    );
    fn webd_engine_blocking_forget(view: u64, list: *const c_char);
}

/// `webd_engine_start_host`: create the WebKit host with its children started in `children` (the
/// environment decided once: webd_abi.h). Once, on the thread that runs WebKit's loop.
///
/// # Safety
/// The header's: once, before the run loop turns, on that thread.
pub unsafe fn start_host(children: &Path) -> Result<(), i32> {
    let dir = CString::new(children.to_string_lossy().as_bytes()).map_err(|_| -1)?;
    let config = WebdHostConfig {
        struct_size: std::mem::size_of::<WebdHostConfig>() as u32,
        children_directory: dir.as_ptr(),
    };
    // SAFETY: `config` and its string live across the call; the caller's contract is the header's.
    match unsafe { webd_engine_start_host(&config) } {
        0 => Ok(()),
        code => Err(code),
    }
}

// The calls: each copies its arguments (the engine only queues work) and returns.
// SAFETY (every block below): the strings live across the call and the engine copies them.

pub fn load(view: u64, url: &str) {
    let url = c_string(url);
    unsafe { webd_engine_load(view, url.as_ptr()) }
}
pub fn reload(view: u64) {
    unsafe { webd_engine_reload(view) }
}
pub fn stop(view: u64) {
    unsafe { webd_engine_stop(view) }
}
pub fn back(view: u64) {
    unsafe { webd_engine_back(view) }
}
pub fn forward(view: u64) {
    unsafe { webd_engine_forward(view) }
}
pub fn download(view: u64, url: &str) {
    let url = c_string(url);
    unsafe { webd_engine_download(view, url.as_ptr()) }
}
pub fn download_cancel(view: u64, download: u64) {
    unsafe { webd_engine_download_cancel(view, download) }
}
pub fn dialog_reply(view: u64, dialog: u64, accepted: bool, value: &str) {
    let value = c_string(value);
    unsafe { webd_engine_dialog_reply(view, dialog, c_int::from(accepted), value.as_ptr()) }
}
pub fn navigation_answer(view: u64, request: u64, allow: bool) {
    unsafe { webd_engine_navigation_answer(view, request, c_int::from(allow)) }
}
pub fn notify_shown(id: u64) {
    unsafe { webd_engine_notify_shown(id) }
}
pub fn notify_clicked(id: u64) {
    unsafe { webd_engine_notify_clicked(id) }
}
pub fn notify_closed(id: u64) {
    unsafe { webd_engine_notify_closed(id) }
}
pub fn notify_permission(view: u64, origin: &str, answer: i32) {
    let origin = c_string(origin);
    unsafe { webd_engine_notify_permission(view, origin.as_ptr(), answer) }
}
pub fn blocking_load(view: u64, list: &str) {
    let list = c_string(list);
    unsafe { webd_engine_blocking_load(view, list.as_ptr()) }
}
pub fn blocking_compile(view: u64, list: &str, json: &[u8]) {
    let list = c_string(list);
    unsafe { webd_engine_blocking_compile(view, list.as_ptr(), json.as_ptr().cast(), json.len()) }
}
pub fn blocking_forget(view: u64, list: &str) {
    let list = c_string(list);
    unsafe { webd_engine_blocking_forget(view, list.as_ptr()) }
}
