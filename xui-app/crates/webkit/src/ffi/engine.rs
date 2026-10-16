//! The engine's functions (`lzw_engine_*` in `webkit/port/app/lazyos_ui.h`) behind the [`Engine`]
//! trait: each queues work for WebKit's main thread and returns.

use std::ffi::{c_char, c_int, c_void};

use crate::abi::c_string;
use crate::engine::{Button, Engine, MouseKind, NotifyAnswer};
use crate::pointer::{Coalescer, Move};

extern "C" {
    fn lzw_engine_load(url: *const c_char);
    fn lzw_engine_reload();
    fn lzw_engine_stop();
    fn lzw_engine_recover(reload: c_int);
    fn lzw_engine_back();
    fn lzw_engine_forward();
    fn lzw_engine_resize(width: c_int, height: c_int);
    fn lzw_engine_mouse(kind: c_int, button: c_int, x: c_int, y: c_int, mods: u32);
    fn lzw_engine_mouse_now(kind: c_int, button: c_int, x: c_int, y: c_int, mods: u32);
    fn lzw_engine_run_on_main(task: extern "C" fn(*mut c_void), context: *mut c_void);
    fn lzw_engine_wheel(x: c_int, y: c_int, ticks_x: c_int, ticks_y: c_int, mods: u32);
    fn lzw_engine_key(press: c_int, keysym: u32, text: *const c_char, mods: u32);
    fn lzw_engine_focus(focused: c_int);
    fn lzw_engine_download(url: *const c_char);
    fn lzw_engine_download_cancel(id: u64);
    fn lzw_engine_dialog_reply(id: u64, accepted: c_int, value: *const c_char);
    fn lzw_engine_notify_shown(id: u64);
    fn lzw_engine_notify_clicked(id: u64);
    fn lzw_engine_notify_closed(id: u64);
    fn lzw_engine_notify_permission(origin: *const c_char, answer: c_int);
    fn lzw_engine_notify_forget(origin: *const c_char);
    fn lzw_engine_blocking_load(id: *const c_char);
    fn lzw_engine_blocking_compile(id: *const c_char, json: *const c_char);
    fn lzw_engine_blocking_forget(id: *const c_char);
}

/// The pointer moves waiting for WebKit's main thread (issue #890): one slot, one task.
static MOVES: Coalescer = Coalescer::new();

/// On WebKit's main thread: the task queued for turn `context` of [`MOVES`] delivers the latest move.
extern "C" fn deliver_move(context: *mut c_void) {
    if let Some(update) = MOVES.drain(context as u64) {
        // SAFETY: plain integers; this runs on the main thread, as the function requires.
        unsafe {
            lzw_engine_mouse_now(
                MouseKind::Move.code(),
                update.button.code(),
                update.x,
                update.y,
                update.mods,
            )
        }
    }
}

/// Queues the move still waiting in the slot ahead of another pointer event, so the page sees the
/// pointer where the press, release or wheel happened.
fn flush_move() {
    if let Some(update) = MOVES.take() {
        // SAFETY: plain integers.
        unsafe {
            lzw_engine_mouse(
                MouseKind::Move.code(),
                update.button.code(),
                update.x,
                update.y,
                update.mods,
            )
        }
    }
}

/// The engine behind the C++ functions.
pub struct FfiEngine;

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
    fn recover(&self, reload: bool) {
        // SAFETY: a plain integer.
        unsafe { lzw_engine_recover(c_int::from(reload)) }
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
        if matches!(kind, MouseKind::Move) {
            let update = Move { button, x, y, mods };
            if let Some(turn) = MOVES.offer(update) {
                // SAFETY: `deliver_move` takes the turn number back as the context; no pointer is
                // dereferenced.
                unsafe { lzw_engine_run_on_main(deliver_move, turn as *mut c_void) }
            }
            return;
        }
        crate::gesture::stamp();
        flush_move();
        // SAFETY: plain integers.
        unsafe { lzw_engine_mouse(kind.code(), button.code(), x, y, mods) }
    }
    fn wheel(&self, x: i32, y: i32, ticks_x: i32, ticks_y: i32, mods: u32) {
        flush_move();
        // SAFETY: plain integers.
        unsafe { lzw_engine_wheel(x, y, ticks_x, ticks_y, mods) }
    }
    fn key(&self, press: bool, keysym: u32, text: &str, mods: u32) {
        if press {
            crate::gesture::stamp();
        }
        let text = c_string(text);
        // SAFETY: `text` is a NUL-terminated string that outlives the call.
        unsafe { lzw_engine_key(c_int::from(press), keysym, text.as_ptr(), mods) }
    }
    fn focus(&self, focused: bool) {
        // SAFETY: a plain integer.
        unsafe { lzw_engine_focus(c_int::from(focused)) }
    }
    fn download(&self, url: &str) {
        let url = c_string(url);
        // SAFETY: `url` is a NUL-terminated string that outlives the call; the C++ side copies it.
        unsafe { lzw_engine_download(url.as_ptr()) }
    }
    fn download_cancel(&self, id: u64) {
        // SAFETY: a plain integer.
        unsafe { lzw_engine_download_cancel(id) }
    }
    fn dialog_reply(&self, id: u64, accepted: bool, value: &str) {
        let value = c_string(value);
        // SAFETY: `value` is a NUL-terminated string that outlives the call; the C++ side copies it.
        unsafe { lzw_engine_dialog_reply(id, c_int::from(accepted), value.as_ptr()) }
    }
    fn notify_shown(&self, id: u64) {
        // SAFETY: a plain integer.
        unsafe { lzw_engine_notify_shown(id) }
    }
    fn notify_clicked(&self, id: u64) {
        // SAFETY: a plain integer.
        unsafe { lzw_engine_notify_clicked(id) }
    }
    fn notify_closed(&self, id: u64) {
        // SAFETY: a plain integer.
        unsafe { lzw_engine_notify_closed(id) }
    }
    fn notify_permission(&self, origin: &str, answer: NotifyAnswer) {
        let origin = c_string(origin);
        // SAFETY: `origin` is a NUL-terminated string that outlives the call; the C++ side copies it.
        unsafe { lzw_engine_notify_permission(origin.as_ptr(), answer.code()) }
    }
    fn notify_forget(&self, origin: &str) {
        let origin = c_string(origin);
        // SAFETY: `origin` is a NUL-terminated string that outlives the call; the C++ side copies it.
        unsafe { lzw_engine_notify_forget(origin.as_ptr()) }
    }
    fn blocking_load(&self, id: &str) {
        let id = c_string(id);
        // SAFETY: `id` is a NUL-terminated string that outlives the call; the C++ side copies it.
        unsafe { lzw_engine_blocking_load(id.as_ptr()) }
    }
    fn blocking_compile(&self, id: &str, json: &str) {
        let (id, json) = (c_string(id), c_string(json));
        // SAFETY: both are NUL-terminated strings that outlive the call; the C++ side copies them.
        unsafe { lzw_engine_blocking_compile(id.as_ptr(), json.as_ptr()) }
    }
    fn blocking_forget(&self, id: &str) {
        let id = c_string(id);
        // SAFETY: `id` is a NUL-terminated string that outlives the call; the C++ side copies it.
        unsafe { lzw_engine_blocking_forget(id.as_ptr()) }
    }
}
