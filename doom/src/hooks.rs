//! The six platform hooks doomgeneric calls (`doomgeneric.h`), routed to the
//! active backend.
//!
//! The engine is single-threaded and calls every hook from the thread that
//! ticks it, so the backend lives in a thread-local; a hook called before
//! [`install`] (or on another thread) finds no backend and does nothing.

use std::cell::RefCell;
use std::ffi::{c_char, c_int, CStr};
use std::time::Instant;

use crate::engine::{HEIGHT, WIDTH};
use crate::headless::Headless;
use crate::window::Window;

extern "C" {
    /// The engine's frame, `WIDTH * HEIGHT` XRGB words (allocated by
    /// `doomgeneric_Create` before it calls `DG_Init`).
    static DG_ScreenBuffer: *mut u32;
}

/// Where frames and keys go.
pub enum Backend {
    Window(Window),
    Headless(Headless),
}

thread_local! {
    static BACKEND: RefCell<Option<Backend>> = const { RefCell::new(None) };
    static START: Instant = Instant::now();
}

/// Make `backend` the target of every hook.
pub fn install(backend: Backend) {
    START.with(|_| {}); // the tick clock starts now, not at the first tick
    BACKEND.with(|slot| *slot.borrow_mut() = Some(backend));
}

fn with_backend(f: impl FnOnce(&mut Backend)) {
    BACKEND.with(|slot| {
        if let Some(backend) = slot.borrow_mut().as_mut() {
            f(backend);
        }
    });
}

/// The engine's current frame, or `None` before it allocated one.
fn frame() -> Option<&'static [u32]> {
    // SAFETY: `DG_ScreenBuffer` is a `malloc` of `WIDTH * HEIGHT` words made
    // once by `doomgeneric_Create` and never freed; the engine only writes it
    // inside `doomgeneric_Tick`, which is not running while a hook reads it.
    unsafe {
        let pixels = DG_ScreenBuffer;
        (!pixels.is_null()).then(|| core::slice::from_raw_parts(pixels, WIDTH * HEIGHT))
    }
}

#[no_mangle]
pub extern "C" fn DG_Init() {}

#[no_mangle]
pub extern "C" fn DG_DrawFrame() {
    let Some(pixels) = frame() else {
        return;
    };
    with_backend(|backend| match backend {
        Backend::Window(window) => window.draw(pixels, WIDTH, HEIGHT),
        Backend::Headless(headless) => headless.draw(pixels),
    });
}

#[no_mangle]
pub extern "C" fn DG_SleepMs(ms: u32) {
    xui_app::sys::sleep_millis(u64::from(ms));
}

#[no_mangle]
pub extern "C" fn DG_GetTicksMs() -> u32 {
    START.with(|start| start.elapsed().as_millis() as u32)
}

/// Hand the engine the oldest key transition: `1` with `*pressed` and `*key`
/// filled, or `0` when there is none. The window pumps its event channels
/// first, so input never waits for a frame.
#[no_mangle]
pub extern "C" fn DG_GetKey(pressed: *mut c_int, key: *mut u8) -> c_int {
    let mut next = None;
    with_backend(|backend| {
        if let Backend::Window(window) = backend {
            window.pump();
            next = window.keys.pop();
        }
    });
    let Some((down, code)) = next else {
        return 0;
    };
    if pressed.is_null() || key.is_null() {
        return 0;
    }
    // SAFETY: the engine passes pointers to two locals of `I_GetEvent`.
    unsafe {
        *pressed = c_int::from(down);
        *key = code;
    }
    1
}

#[no_mangle]
pub extern "C" fn DG_SetWindowTitle(title: *const c_char) {
    if title.is_null() {
        return;
    }
    // SAFETY: the engine passes a NUL-terminated string it owns.
    let title = unsafe { CStr::from_ptr(title) }.to_string_lossy();
    with_backend(|backend| {
        if let Backend::Window(window) = backend {
            window.set_title(&title);
        }
    });
}
