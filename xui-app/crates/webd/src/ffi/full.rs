//! The full views' half of the C ABI (`webd_abi.h`, "P2 slice 3"): the `webd_engine_*` calls the
//! service makes, and the `webd_ui_*` / `webd_paint_*` callbacks the engine makes on WebKit's main
//! thread, forwarded to the installed [`EventQueue`](crate::events::EventQueue). The pure decisions
//! (`webd_ui_navigate`, `webd_ui_blocking`, `webd_ui_download_start`, `webd_paint_plan`) answer from
//! the queue's rules at once.

use std::ffi::{c_char, c_int, CString};
use std::path::Path;

use super::{c_string, sink, text};
use crate::events::rules::PaintPlan;
use crate::events::PageNow;

#[repr(C)]
struct WebdHostConfig {
    struct_size: u32,
    children_directory: *const c_char,
}

/// `webd_page_state`, field for field.
#[repr(C)]
pub struct WebdPageState {
    pub struct_size: u32,
    pub url: *const c_char,
    pub title: *const c_char,
    pub loading: i32,
    pub can_back: i32,
    pub can_forward: i32,
    pub progress: i32,
    pub error: *const c_char,
    pub committed_url: *const c_char,
    pub secure: i32,
    pub finished: i32,
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

// ---- the callbacks (WebKit's main thread) ----

/// `count` rectangles of four ints at `rects`, or none for a null pointer or a bad count.
///
/// # Safety
/// `rects` is NULL or valid for `count * 4` ints for the call.
unsafe fn rects_of(rects: *const c_int, count: c_int) -> Vec<[i32; 4]> {
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    if rects.is_null() || count == 0 || count > 64 {
        return Vec::new();
    }
    // SAFETY: by the contract above.
    let ints = unsafe { std::slice::from_raw_parts(rects, count * 4) };
    ints.chunks_exact(4)
        .map(|r| [r[0], r[1], r[2], r[3]])
        .collect()
}

/// `width * height * 4` bytes at `rgba`, or `None` for a null pointer or a bad size.
///
/// # Safety
/// `rgba` is NULL or valid for that many bytes for the call.
unsafe fn pixels<'a>(rgba: *const u8, width: c_int, height: c_int) -> Option<&'a [u8]> {
    let len = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(4)?;
    if rgba.is_null() || len == 0 {
        return None;
    }
    // SAFETY: by the contract above.
    Some(unsafe { std::slice::from_raw_parts(rgba, len) })
}

/// # Safety
/// `state` is NULL or a `webd_page_state` whose strings are NULL or NUL-terminated, valid for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_page_state(view: u64, state: *const WebdPageState) {
    let (Some(sink), false) = (sink(), state.is_null()) else {
        return;
    };
    // SAFETY: by the contract above; a caller's older, shorter struct is not supported (one header).
    let s = unsafe { &*state };
    if (s.struct_size as usize) < std::mem::size_of::<WebdPageState>() {
        return;
    }
    // SAFETY: as above.
    let page = unsafe {
        PageNow {
            url: text(s.url).into_owned(),
            title: text(s.title).into_owned(),
            loading: s.loading != 0,
            can_back: s.can_back != 0,
            can_forward: s.can_forward != 0,
            progress: u32::try_from(s.progress).unwrap_or(0),
            error: text(s.error).into_owned(),
            committed_url: text(s.committed_url).into_owned(),
            secure: s.secure != 0,
            finished: u32::try_from(s.finished).unwrap_or(0),
        }
    };
    sink.page_state(view, page);
}

#[no_mangle]
pub extern "C" fn webd_paint_resize(view: u64, width: c_int, height: c_int) {
    if let Some(sink) = sink() {
        sink.paint_resize(view, width, height);
    }
}

#[no_mangle]
pub extern "C" fn webd_paint_full(view: u64) {
    if let Some(sink) = sink() {
        sink.paint_full(view);
    }
}

#[no_mangle]
pub extern "C" fn webd_paint_rect(view: u64, x: c_int, y: c_int, width: c_int, height: c_int) {
    if let Some(sink) = sink() {
        sink.paint_rect(view, [x, y, width, height]);
    }
}

/// # Safety
/// `rects` is NULL or valid for `count * 4` ints for the call.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn webd_paint_update(
    view: u64,
    sx: c_int,
    sy: c_int,
    sw: c_int,
    sh: c_int,
    dx: c_int,
    dy: c_int,
    rects: *const c_int,
    count: c_int,
) {
    if let Some(sink) = sink() {
        // SAFETY: forwarded.
        let list = unsafe { rects_of(rects, count) };
        if count > 0 && list.is_empty() {
            // More than a plan can say: all of it.
            sink.paint_full(view);
            return;
        }
        sink.paint_update(view, [sx, sy, sw, sh], dx, dy, &list);
    }
}

/// `WEBD_PAINT_*`; `scroll` gets six ints (zero when nothing moved), `rects` up to `max` rectangles.
///
/// # Safety
/// `scroll` is valid for 6 ints, `rects` for `max * 4`, `count` for one, for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_paint_plan(
    view: u64,
    scroll: *mut c_int,
    rects: *mut c_int,
    max: c_int,
    count: *mut c_int,
) -> c_int {
    let Some(sink) = sink() else {
        return 1;
    };
    if scroll.is_null() || rects.is_null() || count.is_null() {
        return 1;
    }
    // SAFETY: by the contract above.
    unsafe {
        *count = 0;
        std::slice::from_raw_parts_mut(scroll, 6).fill(0);
    }
    match sink.paint_plan(view) {
        PaintPlan::Nothing => 0,
        PaintPlan::Full => 1,
        PaintPlan::Delta { shift, rects: list } => {
            let max = usize::try_from(max).unwrap_or(0);
            if list.len() > max {
                return 1;
            }
            // SAFETY: as above; `list.len() <= max`.
            unsafe {
                if let Some(shift) = shift {
                    std::slice::from_raw_parts_mut(scroll, 6).copy_from_slice(&shift);
                }
                let out = std::slice::from_raw_parts_mut(rects, max * 4);
                for (slot, rect) in out.chunks_exact_mut(4).zip(&list) {
                    slot.copy_from_slice(rect);
                }
                *count = list.len() as c_int;
            }
            2
        }
    }
}

/// Non-zero: refused (the engine paints a whole frame next).
///
/// # Safety
/// `rgba` valid for `width * height * 4` bytes, `scroll` NULL or six ints, `rects` `count * 4` ints.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_frame_delta(
    view: u64,
    rgba: *const u8,
    width: c_int,
    height: c_int,
    scroll: *const c_int,
    rects: *const c_int,
    count: c_int,
) -> c_int {
    let Some(sink) = sink() else {
        return 1;
    };
    // SAFETY: forwarded from the header's contract.
    let (Some(pixels), list) = (unsafe { pixels(rgba, width, height) }, unsafe {
        rects_of(rects, count)
    }) else {
        return 1;
    };
    let shift = (!scroll.is_null()).then(|| {
        // SAFETY: six ints when not NULL.
        let s = unsafe { std::slice::from_raw_parts(scroll, 6) };
        [s[0], s[1], s[2], s[3], s[4], s[5]]
    });
    c_int::from(sink.frame_delta(view, pixels, width, height, shift, &list))
}

/// # Safety
/// `rgba` valid for `width * height * 4` bytes for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_band(
    view: u64,
    doc_x: c_int,
    doc_y: c_int,
    width: c_int,
    height: c_int,
    rgba: *const u8,
) {
    if let Some(sink) = sink() {
        // SAFETY: forwarded.
        if let Some(pixels) = unsafe { pixels(rgba, width, height) } {
            sink.band(view, doc_x, doc_y, width, height, pixels);
        }
    }
}

/// # Safety
/// `url` NULL or NUL-terminated for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_hover(view: u64, url: *const c_char) {
    if let Some(sink) = sink() {
        sink.hover(view, &unsafe { text(url) });
    }
}

#[no_mangle]
pub extern "C" fn webd_ui_cursor(view: u64, shape: c_int) {
    if let Some(sink) = sink() {
        sink.cursor(view, shape);
    }
}

/// # Safety
/// `url` NULL or NUL-terminated for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_new_window(view: u64, url: *const c_char, gesture: c_int) {
    if let Some(sink) = sink() {
        sink.new_window(view, &unsafe { text(url) }, gesture != 0);
    }
}

#[no_mangle]
pub extern "C" fn webd_ui_responsive(view: u64, responsive: c_int) {
    if let Some(sink) = sink() {
        sink.responsive(view, responsive != 0);
    }
}

/// # Safety
/// The strings NULL or NUL-terminated for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_dialog(
    view: u64,
    dialog: u64,
    kind: c_int,
    message: *const c_char,
    default: *const c_char,
) {
    if let Some(sink) = sink() {
        let (message, default) = unsafe { (text(message), text(default)) };
        sink.dialog(view, dialog, kind, &message, &default);
    }
}

/// 0 with the path written; non-zero refuses.
///
/// # Safety
/// The strings NULL or NUL-terminated; `path` valid for `path_size` bytes; for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_download_start(
    view: u64,
    download: u64,
    url: *const c_char,
    suggested: *const c_char,
    total: i64,
    path: *mut c_char,
    path_size: usize,
) -> c_int {
    let (Some(sink), false) = (sink(), path.is_null()) else {
        return 1;
    };
    let (url, suggested) = unsafe { (text(url), text(suggested)) };
    let Some(part) = sink.download_start(view, download, &url, &suggested, total) else {
        return 1;
    };
    let bytes = part.as_bytes();
    if bytes.len() >= path_size || bytes.contains(&0) {
        return 1;
    }
    // SAFETY: `path` holds `path_size` bytes and the path and its NUL fit.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), path.cast::<u8>(), bytes.len());
        *path.add(bytes.len()) = 0;
    }
    0
}

#[no_mangle]
pub extern "C" fn webd_ui_download_progress(view: u64, download: u64, received: i64, total: i64) {
    if let Some(sink) = sink() {
        sink.download_progress(view, download, received, total);
    }
}

#[no_mangle]
pub extern "C" fn webd_ui_download_done(view: u64, download: u64) {
    if let Some(sink) = sink() {
        sink.download_done(view, download);
    }
}

/// # Safety
/// `reason` NULL or NUL-terminated for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_download_fail(view: u64, download: u64, reason: *const c_char) {
    if let Some(sink) = sink() {
        sink.download_fail(view, download, &unsafe { text(reason) });
    }
}

/// `WEBD_VERDICT_*`.
///
/// # Safety
/// The strings NULL or NUL-terminated for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_navigate(
    view: u64,
    request: u64,
    url: *const c_char,
    kind: c_int,
    gesture: c_int,
    main_frame: c_int,
    _committed: *const c_char,
) -> c_int {
    let Some(sink) = sink() else {
        return 1;
    };
    let url = unsafe { text(url) };
    sink.navigate(view, request, &url, kind, gesture != 0, main_frame != 0) as c_int
}

/// # Safety
/// The strings NULL or NUL-terminated for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_notify_show(
    view: u64,
    id: u64,
    origin: *const c_char,
    _page_url: *const c_char,
    title: *const c_char,
    body: *const c_char,
    _tag: *const c_char,
) {
    if let Some(sink) = sink() {
        let (origin, title, body) = unsafe { (text(origin), text(title), text(body)) };
        sink.notify_show(view, id, &origin, &title, &body);
    }
}

#[no_mangle]
pub extern "C" fn webd_ui_notify_cancel(view: u64, id: u64) {
    if let Some(sink) = sink() {
        sink.notify_cancel(view, id);
    }
}

#[no_mangle]
pub extern "C" fn webd_ui_notify_gone(view: u64, id: u64) {
    if let Some(sink) = sink() {
        sink.notify_gone(view, id);
    }
}

/// # Safety
/// `origin` NULL or NUL-terminated for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_notify_permission_request(view: u64, origin: *const c_char) {
    if let Some(sink) = sink() {
        sink.notify_permission_request(view, &unsafe { text(origin) });
    }
}

/// # Safety
/// `origin` NULL or NUL-terminated for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_notify_permission_gone(view: u64, origin: *const c_char) {
    if let Some(sink) = sink() {
        sink.notify_permission_gone(view, &unsafe { text(origin) });
    }
}

/// Non-zero: the rule lists apply.
///
/// # Safety
/// `url` NULL or NUL-terminated for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_blocking(view: u64, url: *const c_char) -> c_int {
    let Some(sink) = sink() else {
        return 1;
    };
    c_int::from(sink.blocking(view, &unsafe { text(url) }))
}

/// # Safety
/// `list` NULL or NUL-terminated for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_blocking_list(view: u64, list: *const c_char, status: c_int) {
    if let Some(sink) = sink() {
        sink.blocking_list(view, &unsafe { text(list) }, status);
    }
}

#[no_mangle]
pub extern "C" fn webd_ui_blocked(view: u64, count: c_int) {
    if let Some(sink) = sink() {
        sink.blocked(view, count);
    }
}
