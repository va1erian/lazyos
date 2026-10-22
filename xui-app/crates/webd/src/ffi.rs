//! The C ABI of webd's WebKit half (`webkit/port/webd/webd_abi.h`), from the Rust side (feature
//! `host`; nothing links it yet, slice 2 does).
//!
//! * [`FfiEngine`] is an [`Engine`] over the `webd_engine_*` functions the C++ half implements.
//! * The `webd_ui_*` functions below are the callbacks the C++ half calls on WebKit's main thread.
//!   They forward to one global [`EngineEvents`] sink ([`install`]); until one is installed they ignore
//!   what they are told. The strings and the pixels are valid only until the call returns, so the sink
//!   copies (the [`crate::events::EventQueue`] does).
//!
//! The declarations are written by hand against the header: a change to the header is a change here,
//! and `layout` pins `webd_view_config`.

use std::borrow::Cow;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::{Arc, RwLock};

use crate::engine::{Engine, EngineConfig, EngineEvents};
use crate::events::EventQueue;

mod full;
pub use full::start_host;

/// `webd_view_config`, field for field. Every field is read once, when the view is created.
#[repr(C)]
#[derive(Debug)]
pub struct WebdViewConfig {
    /// `sizeof(webd_view_config)` as this side knows it.
    pub struct_size: u32,
    /// 1: a light view. Slice 1 accepts only 1.
    pub light: i32,
    pub scripts: i32,
    pub width: i32,
    pub height: i32,
    pub group: i32,
    pub color_scheme: i32,
    pub default_font_size: i32,
    /// UTF-8, NUL-terminated, or NULL for none.
    pub user_style: *const c_char,
    // ---- full views (read when `light` is 0) ----
    pub features: i32,
    /// The profile's directory, or NULL for a store in memory.
    pub profile: *const c_char,
    pub memory_only: i32,
    pub deltas: i32,
    pub user_agent_suffix: *const c_char,
    pub no_network: i32,
}

extern "C" {
    fn webd_engine_start() -> c_int;
    fn webd_engine_view_create(view: u64, config: *const WebdViewConfig);
    fn webd_engine_view_destroy(view: u64);
    fn webd_engine_load_html(view: u64, html: *const u8, len: usize, base_url: *const c_char);
    fn webd_engine_resize(view: u64, width: c_int, height: c_int);
    fn webd_engine_set_visible(view: u64, visible: c_int);
    fn webd_engine_refresh(view: u64);
    fn webd_engine_mouse(view: u64, kind: c_int, button: c_int, x: c_int, y: c_int, mods: u32);
    fn webd_engine_wheel(view: u64, x: c_int, y: c_int, ticks_x: c_int, ticks_y: c_int, mods: u32);
    fn webd_engine_key(view: u64, press: c_int, keysym: u32, text: *const c_char, mods: u32);
    fn webd_engine_focus(view: u64, focused: c_int);
    fn webd_engine_run_on_main(task: extern "C" fn(*mut c_void), context: *mut c_void);
    fn webd_engine_quit();
    fn webd_engine_children(web: *mut c_int, network: *mut c_int);
}

/// A C string for an optional text: `None` for an empty one (the engine reads NULL as none).
fn optional(text: &str) -> Option<CString> {
    (!text.is_empty()).then(|| c_string(text))
}

fn ptr(text: &Option<CString>) -> *const c_char {
    text.as_ref().map_or(std::ptr::null(), |s| s.as_ptr())
}

/// A C string for the engine. The service already refuses text with a NUL, so this never cuts.
fn c_string(text: &str) -> CString {
    CString::new(text).unwrap_or_default()
}

/// The engine over the C functions. Zero-sized: the engine is the process's.
#[derive(Clone, Copy, Debug, Default)]
pub struct FfiEngine;

impl FfiEngine {
    /// `webd_engine_start`: create the WebKit host. Once, on the process's main thread, before the
    /// run loop turns. `Err` carries the C function's non-zero result.
    ///
    /// # Safety
    /// Must be called on the thread the C++ half takes as WebKit's main thread, once.
    pub unsafe fn start() -> Result<(), i32> {
        // SAFETY: the caller's contract is the header's.
        match unsafe { webd_engine_start() } {
            0 => Ok(()),
            code => Err(code),
        }
    }

    /// `webd_engine_run_on_main`: run `task(context)` on WebKit's main thread, in order with the other calls.
    ///
    /// # Safety
    /// `context` must stay valid until `task` has run, and `task` must be safe to run on that thread with it.
    pub unsafe fn run_on_main(task: extern "C" fn(*mut c_void), context: *mut c_void) {
        // SAFETY: forwarded; the contract is the caller's.
        unsafe { webd_engine_run_on_main(task, context) }
    }
}

impl Engine for FfiEngine {
    fn view_create(&self, view: u64, config: &EngineConfig) {
        let style = optional(&config.user_style);
        let full = config.full.clone().unwrap_or_default();
        let profile = optional(&full.profile);
        let suffix = optional(&full.user_agent_suffix);
        let raw = WebdViewConfig {
            struct_size: std::mem::size_of::<WebdViewConfig>() as u32,
            light: i32::from(config.full.is_none()),
            scripts: i32::from(config.scripts),
            width: config.width,
            height: config.height,
            group: config.group,
            color_scheme: config.color_scheme,
            default_font_size: config.default_font_size,
            user_style: ptr(&style),
            features: full.features,
            profile: ptr(&profile),
            memory_only: i32::from(full.memory_only),
            deltas: i32::from(full.deltas),
            user_agent_suffix: ptr(&suffix),
            no_network: i32::from(full.no_network),
        };
        // SAFETY: `raw` and its strings live across the call; the engine reads them only within it.
        unsafe { webd_engine_view_create(view, &raw) }
    }

    fn view_destroy(&self, view: u64) {
        // SAFETY: plain values.
        unsafe { webd_engine_view_destroy(view) }
    }

    fn load_html(&self, view: u64, html: &[u8], base_url: &str) {
        let base = c_string(base_url);
        // SAFETY: the engine copies `html` and the base before returning.
        unsafe { webd_engine_load_html(view, html.as_ptr(), html.len(), base.as_ptr()) }
    }

    fn resize(&self, view: u64, width: i32, height: i32) {
        // SAFETY: plain values.
        unsafe { webd_engine_resize(view, width, height) }
    }

    fn set_visible(&self, view: u64, visible: bool) {
        // SAFETY: plain values.
        unsafe { webd_engine_set_visible(view, c_int::from(visible)) }
    }

    fn refresh(&self, view: u64) {
        // SAFETY: plain values.
        unsafe { webd_engine_refresh(view) }
    }

    fn mouse(&self, view: u64, kind: i32, button: i32, x: i32, y: i32, mods: u32) {
        // SAFETY: plain values.
        unsafe { webd_engine_mouse(view, kind, button, x, y, mods) }
    }

    fn wheel(&self, view: u64, x: i32, y: i32, ticks_x: i32, ticks_y: i32, mods: u32) {
        // SAFETY: plain values.
        unsafe { webd_engine_wheel(view, x, y, ticks_x, ticks_y, mods) }
    }

    fn key(&self, view: u64, press: bool, keysym: u32, text: &str, mods: u32) {
        let text = c_string(text);
        // SAFETY: the engine reads the string only within the call.
        unsafe { webd_engine_key(view, c_int::from(press), keysym, text.as_ptr(), mods) }
    }

    fn focus(&self, view: u64, focused: bool) {
        // SAFETY: plain values.
        unsafe { webd_engine_focus(view, c_int::from(focused)) }
    }

    fn quit(&self) {
        // SAFETY: no arguments.
        unsafe { webd_engine_quit() }
    }

    fn children(&self) -> (i32, i32) {
        let (mut web, mut network) = (0, 0);
        // SAFETY: two valid out-pointers.
        unsafe { webd_engine_children(&mut web, &mut network) };
        (web, network)
    }

    fn load(&self, view: u64, url: &str) {
        full::load(view, url)
    }
    fn reload(&self, view: u64) {
        full::reload(view)
    }
    fn stop(&self, view: u64) {
        full::stop(view)
    }
    fn back(&self, view: u64) {
        full::back(view)
    }
    fn forward(&self, view: u64) {
        full::forward(view)
    }
    fn download(&self, view: u64, url: &str) {
        full::download(view, url)
    }
    fn download_cancel(&self, view: u64, download: u64) {
        full::download_cancel(view, download)
    }
    fn dialog_reply(&self, view: u64, dialog: u64, accepted: bool, value: &str) {
        full::dialog_reply(view, dialog, accepted, value)
    }
    fn navigation_answer(&self, view: u64, request: u64, allow: bool) {
        full::navigation_answer(view, request, allow)
    }
    fn notify_shown(&self, id: u64) {
        full::notify_shown(id)
    }
    fn notify_clicked(&self, id: u64) {
        full::notify_clicked(id)
    }
    fn notify_closed(&self, id: u64) {
        full::notify_closed(id)
    }
    fn notify_permission(&self, view: u64, origin: &str, answer: i32) {
        full::notify_permission(view, origin, answer)
    }
    fn blocking_load(&self, view: u64, list: &str) {
        full::blocking_load(view, list)
    }
    fn blocking_compile(&self, view: u64, list: &str, json: &[u8]) {
        full::blocking_compile(view, list, json)
    }
    fn blocking_forget(&self, view: u64, list: &str) {
        full::blocking_forget(view, list)
    }
}

static SINK: RwLock<Option<Arc<EventQueue>>> = RwLock::new(None);

/// Make `sink` the receiver of the `webd_ui_*` and `webd_paint_*` callbacks, replacing any earlier
/// one. Call before the engine starts.
pub fn install(sink: Arc<EventQueue>) {
    *SINK
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(sink);
}

fn sink() -> Option<Arc<EventQueue>> {
    SINK.read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// A C string argument as text; NULL is `""`, bytes that are not UTF-8 become U+FFFD.
///
/// # Safety
/// `text` is NULL or a NUL-terminated string valid for the call.
unsafe fn text<'a>(text: *const c_char) -> Cow<'a, str> {
    if text.is_null() {
        return Cow::Borrowed("");
    }
    // SAFETY: by the contract above.
    unsafe { CStr::from_ptr(text) }.to_string_lossy()
}

/// `webd_ui_state`.
///
/// # Safety
/// The strings are NULL or NUL-terminated and valid for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_state(
    view: u64,
    title: *const c_char,
    loading: c_int,
    finished: c_int,
    error: *const c_char,
) {
    if let Some(sink) = sink() {
        // SAFETY: forwarded from the header's contract.
        let (title, error) = unsafe { (text(title), text(error)) };
        sink.state(view, &title, loading != 0, finished, &error);
    }
}

/// `webd_ui_frame`.
///
/// # Safety
/// `rgba` is NULL or valid for `width * height * 4` bytes for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_frame(view: u64, rgba: *const u8, width: c_int, height: c_int) {
    let (Some(sink), false) = (sink(), rgba.is_null()) else {
        return;
    };
    let len = (width.max(0) as usize)
        .checked_mul(height.max(0) as usize)
        .and_then(|pixels| pixels.checked_mul(4));
    let Some(len) = len.filter(|len| *len > 0) else {
        return;
    };
    // SAFETY: the header promises `width * height * 4` bytes at `rgba` until the call returns.
    let pixels = unsafe { std::slice::from_raw_parts(rgba, len) };
    sink.frame(view, pixels, width, height);
}

/// `webd_ui_scroll`.
#[no_mangle]
pub extern "C" fn webd_ui_scroll(view: u64, x: c_int, y: c_int, at_bottom: c_int) {
    if let Some(sink) = sink() {
        sink.scroll(view, x, y, at_bottom != 0);
    }
}

/// `webd_ui_link_clicked`.
///
/// # Safety
/// `url` is NULL or NUL-terminated and valid for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_link_clicked(view: u64, url: *const c_char) {
    if let Some(sink) = sink() {
        // SAFETY: forwarded from the header's contract.
        sink.link_clicked(view, &unsafe { text(url) });
    }
}

/// `webd_ui_clipboard_write`.
///
/// # Safety
/// `copied` is NULL or NUL-terminated and valid for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_clipboard_write(view: u64, copied: *const c_char) {
    if let Some(sink) = sink() {
        // SAFETY: forwarded from the header's contract.
        sink.clipboard_write(view, &unsafe { text(copied) });
    }
}

/// `webd_ui_view_lost`.
#[no_mangle]
pub extern "C" fn webd_ui_view_lost(view: u64, reason: c_int) {
    if let Some(sink) = sink() {
        sink.view_lost(view, reason);
    }
}

/// `webd_ui_say`: any thread.
///
/// # Safety
/// `line` is NULL or NUL-terminated and valid for the call.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_say(line: *const c_char) {
    if let Some(sink) = sink() {
        // SAFETY: forwarded from the header's contract.
        sink.say(&unsafe { text(line) });
    }
}

/// `webd_ui_user_memory`: 0 on success, non-zero when unknown.
///
/// # Safety
/// `used` and `limit` are NULL or valid for a `u64` write.
#[no_mangle]
pub unsafe extern "C" fn webd_ui_user_memory(used: *mut u64, limit: *mut u64) -> c_int {
    let Some((charge, ceiling)) = sink().and_then(|sink| sink.user_memory()) else {
        return 1;
    };
    // SAFETY: by the contract above.
    unsafe {
        if !used.is_null() {
            *used = charge;
        }
        if !limit.is_null() {
            *limit = ceiling;
        }
    }
    0
}

#[cfg(test)]
mod tests;
