//! The Rust half of the C seam (`c/lazyos/lazyos_host.h`): the `lz_*`
//! functions the engine's platform layer calls, routed to the installed
//! [`Backend`].
//!
//! The engine is single-threaded and calls every hook from the thread that
//! runs it, so the backend lives in a thread-local; a hook called before
//! [`install`] (or on another thread) finds no backend and does nothing.

use std::cell::RefCell;
use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::OnceLock;
use std::time::Instant;

use lazysrb2kart::backend::{Backend, Event, Flow, Frame};

/// Largest frame the engine may hand over: `MAXVIDWIDTH x MAXVIDHEIGHT`
/// (1920 x 1200) with room for the row padding.
const MAX_FRAME_BYTES: usize = 1920 * 1200 * 2;

thread_local! {
    static BACKEND: RefCell<Option<Box<dyn Backend>>> = const { RefCell::new(None) };
    static START: Instant = Instant::now();
}

/// The data and home directories as C strings, set once before the engine runs.
static DIRS: OnceLock<(CString, CString)> = OnceLock::new();

/// Make `backend` the target of every hook.
pub fn install(backend: Box<dyn Backend>) {
    START.with(|_| {}); // the fallback clock starts now
    BACKEND.with(|slot| *slot.borrow_mut() = Some(backend));
}

/// Fix the directories `lz_data_dir` and `lz_home_dir` return.
pub fn set_dirs(data: &str, home: &str) {
    let to_c = |text: &str| CString::new(text).unwrap_or_default();
    let _ = DIRS.set((to_c(data), to_c(home)));
}

fn with_backend<T>(f: impl FnOnce(&mut dyn Backend) -> T) -> Option<T> {
    BACKEND.with(|slot| slot.borrow_mut().as_mut().map(|backend| f(backend.as_mut())))
}

fn quit(status: i32) -> ! {
    let _ = with_backend(|backend| backend.close());
    if status == 0 {
        println!("SRB2KART:QUIT:PASS");
    } else {
        println!("SRB2KART:QUIT:FAIL status={status}");
    }
    std::process::exit(status)
}

/// One engine output line (`I_OutputMsg`).
#[no_mangle]
pub extern "C" fn lz_log(line: *const c_char) {
    if line.is_null() {
        return;
    }
    // SAFETY: the engine passes a NUL-terminated string it owns.
    let line = unsafe { CStr::from_ptr(line) }.to_string_lossy();
    println!("SRB2KART:LOG: {line}");
}

#[no_mangle]
pub extern "C" fn lz_ticks_ns() -> u64 {
    with_backend(|backend| backend.ticks_ns())
        .unwrap_or_else(|| START.with(|start| start.elapsed().as_nanos() as u64))
}

#[no_mangle]
pub extern "C" fn lz_sleep_us(us: u32) {
    if with_backend(|backend| backend.sleep(us)).is_none() {
        xui_app::sys::sleep_millis(u64::from(us.div_ceil(1000)));
    }
}

#[no_mangle]
pub extern "C" fn lz_video_open(width: c_int, height: c_int, title: *const c_char) -> c_int {
    let (Ok(width), Ok(height)) = (usize::try_from(width), usize::try_from(height)) else {
        return -1;
    };
    let title = if title.is_null() {
        "SRB2Kart".into()
    } else {
        // SAFETY: the engine passes a NUL-terminated string it owns.
        unsafe { CStr::from_ptr(title) }.to_string_lossy()
    };
    match with_backend(|backend| backend.open(width, height, &title)) {
        Some(Ok(())) => 0,
        Some(Err(reason)) => {
            println!("SRB2KART:WINDOW:FAIL {reason}");
            -1
        }
        None => -1,
    }
}

#[no_mangle]
pub extern "C" fn lz_present(
    pixels: *const u8,
    width: c_int,
    height: c_int,
    pitch: c_int,
    palette: *const u8,
) {
    let (Ok(width), Ok(height), Ok(pitch)) = (
        usize::try_from(width),
        usize::try_from(height),
        usize::try_from(pitch),
    ) else {
        return;
    };
    let Some(len) = pitch.checked_mul(height).filter(|len| *len <= MAX_FRAME_BYTES) else {
        return;
    };
    if pixels.is_null() || palette.is_null() || width == 0 || height == 0 || pitch < width {
        return;
    }
    // SAFETY: the engine passes its `screens[0]` (`pitch * height` bytes) and
    // its 768-byte palette, both alive and unchanged for the duration of the
    // call; `len` was bounded above.
    let (pixels, palette) = unsafe {
        (
            core::slice::from_raw_parts(pixels, len),
            &*palette.cast::<[u8; 768]>(),
        )
    };
    let frame = Frame {
        pixels,
        width,
        height,
        pitch,
        palette,
    };
    if let Some(Flow::Exit(status)) = with_backend(|backend| backend.present(&frame)) {
        std::process::exit(status);
    }
}

/// Fill `*out` with the oldest pending event and return 1, or return 0.
#[no_mangle]
pub extern "C" fn lz_poll_event(out: *mut Event) -> c_int {
    if out.is_null() {
        return 0;
    }
    let Some(Some(event)) = with_backend(|backend| backend.poll()) else {
        return 0;
    };
    // SAFETY: the engine passes a pointer to a local `struct lz_event`, which
    // has the layout of `Event`.
    unsafe { out.write(event) };
    1
}

#[no_mangle]
pub extern "C" fn lz_data_dir() -> *const c_char {
    DIRS.get().map_or(c"".as_ptr(), |(data, _)| data.as_ptr())
}

#[no_mangle]
pub extern "C" fn lz_home_dir() -> *const c_char {
    DIRS.get().map_or(c"".as_ptr(), |(_, home)| home.as_ptr())
}

#[no_mangle]
pub extern "C" fn lz_quit(status: c_int) {
    quit(status)
}
