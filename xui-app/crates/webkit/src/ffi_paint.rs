//! The paint half of the C ABI: what the page changed ([`lzw_paint_update`] and friends, kept in
//! [`Pending`]), the plan the next paint asks for ([`lzw_paint_plan`]) and the delta it sends
//! ([`lzw_ui_frame_delta`]). The rules are in `damage.rs`; this only moves ints across the border.

use std::ffi::c_int;

use crate::abi::frame_geometry;
use crate::damage::{IRect, Pending, Plan};
use crate::framing::{delta_from_frame, rects_from_ints, scroll_from_ints};
use crate::mailbox;

/// WebKit painted part of the page: the picture the window has first moves (`scroll`: six ints,
/// `x, y, width, height, dx, dy`, or null for no move), then each of the `count` rectangles
/// (`x, y, width, height` ints in `rects`) is replaced from `rgba`, a whole frame laid out as
/// [`lzw_ui_frame`]'s. The rows of the rectangles are copied before this returns.
///
/// Returns 1 when the window could not take it and a whole frame (`lzw_ui_frame`) must follow:
/// it has no picture yet, is too far behind, or the arguments make no sense.
///
/// # Safety
/// `rgba` must point at `width * height * 4` readable bytes, `scroll` at six ints or be null,
/// `rects` at `count * 4` ints, all for the duration of the call.
#[no_mangle]
pub unsafe extern "C" fn lzw_ui_frame_delta(
    rgba: *const u8,
    width: c_int,
    height: c_int,
    scroll: *const c_int,
    rects: *const c_int,
    count: c_int,
) -> c_int {
    let Some((width, height, len)) = frame_geometry(width, height) else {
        return 1;
    };
    let Ok(count) = usize::try_from(count) else {
        return 1;
    };
    if rgba.is_null() || (count > 0 && rects.is_null()) || count > crate::damage::MAX_RECTS {
        return 1;
    }
    // SAFETY: the caller promises `len` readable bytes, and `count * 4` ints at `rects`, and six
    // at `scroll` when it is not null.
    let (pixels, rects, scroll) = unsafe {
        (
            std::slice::from_raw_parts(rgba, len),
            rects_from_ints(std::slice::from_raw_parts(rects, count * 4)),
            (!scroll.is_null()).then(|| *(scroll as *const [c_int; 6])),
        )
    };
    let scroll = scroll.as_ref().and_then(scroll_from_ints);
    match delta_from_frame(pixels, width, height, scroll, &rects) {
        Some(delta) => c_int::from(mailbox::global().post_delta(delta)),
        None => 1,
    }
}

/// The main frame's scroll offset of the pixels the engine posts next (`lzw_ui_frame` or
/// `lzw_ui_frame_delta`): page pixels, `x` unused (the window shows the picture ahead vertically).
#[no_mangle]
pub extern "C" fn lzw_ui_scroll_offset(_x: c_int, y: c_int) {
    mailbox::global().set_scroll_offset(sane(y));
}

/// What the page changed since the last paint (the updates WebKit reported), kept in the form
/// the next paint uses: [`lzw_paint_plan`]. Only the engine's thread calls these.
static PENDING: std::sync::Mutex<Option<Pending>> = std::sync::Mutex::new(None);

fn with_pending<T>(width: c_int, height: c_int, f: impl FnOnce(&mut Pending) -> T) -> T {
    let mut guard = PENDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let pending = guard.get_or_insert_with(|| Pending::new(width, height));
    f(pending)
}

/// The view is `width` by `height` now: nothing painted before fits.
#[no_mangle]
pub extern "C" fn lzw_paint_resize(width: c_int, height: c_int) {
    with_pending(width, height, |pending| pending.resize(width, height));
}

/// Everything must be painted and sent whole.
#[no_mangle]
pub extern "C" fn lzw_paint_full() {
    with_pending(0, 0, Pending::note_full);
}

/// A rectangle of the view changed (WebKit reported it as needing display).
#[no_mangle]
pub extern "C" fn lzw_paint_rect(x: c_int, y: c_int, width: c_int, height: c_int) {
    with_pending(0, 0, |pending| {
        pending.note_rect(IRect::new(x, y, width, height))
    });
}

/// One update from the page's process: the area `sx, sy, sw, sh` moved by `dx, dy` (a zero offset
/// or an empty area: nothing moved) and the `count` rectangles in `rects` were repainted.
///
/// # Safety
/// `rects` must point at `count * 4` readable ints, or `count` be zero.
#[no_mangle]
pub unsafe extern "C" fn lzw_paint_update(
    sx: c_int,
    sy: c_int,
    sw: c_int,
    sh: c_int,
    dx: c_int,
    dy: c_int,
    rects: *const c_int,
    count: c_int,
) {
    let count = usize::try_from(count).unwrap_or(0);
    // SAFETY: the caller promises `count * 4` readable ints at `rects`.
    let rects = if count == 0 || rects.is_null() {
        Vec::new()
    } else {
        rects_from_ints(unsafe { std::slice::from_raw_parts(rects, count * 4) })
    };
    // What the page's process says is not trusted to be a sane view coordinate: past the limit a
    // scroll is more than any view and becomes a whole repaint, and sums of these cannot overflow.
    let area = IRect::new(sane(sx), sane(sy), sane(sw), sane(sh));
    with_pending(0, 0, |pending| {
        pending.note_update(area, sane(dx), sane(dy), &rects);
    });
}

/// The largest coordinate or scroll the paint path believes (a view is thousands of pixels).
const LIMIT: c_int = 1 << 24;

fn sane(value: c_int) -> c_int {
    value.clamp(-LIMIT, LIMIT)
}

/// What the next paint has to do, forgotten once asked: 0 nothing changed, 1 paint and send the
/// whole view (`lzw_ui_frame`), 2 move `scroll` (six ints, all zero when nothing moves), then
/// repaint the `*count` rectangles written to `rects` (four ints each, at most `max`) and send
/// them (`lzw_ui_frame_delta`).
///
/// # Safety
/// `scroll` must point at six writable ints, `rects` at `max * 4` and `count` at one.
#[no_mangle]
pub unsafe extern "C" fn lzw_paint_plan(
    scroll: *mut c_int,
    rects: *mut c_int,
    max: c_int,
    count: *mut c_int,
) -> c_int {
    let plan = with_pending(0, 0, Pending::take_plan);
    // SAFETY: the caller promises these writable, as documented.
    unsafe {
        *count = 0;
        std::slice::from_raw_parts_mut(scroll, 6).fill(0);
    }
    match plan {
        None => 0,
        Some(Plan::Full) => 1,
        Some(Plan::Delta {
            scroll: moved,
            rects: list,
        }) => {
            let max = usize::try_from(max).unwrap_or(0);
            if list.len() > max {
                // More than the caller has room for (never, with `MAX_RECTS`): whole frame.
                return 1;
            }
            // SAFETY: as above; `list.len() <= max`.
            unsafe {
                if let Some(moved) = moved {
                    let out = std::slice::from_raw_parts_mut(scroll, 6);
                    out.copy_from_slice(&[
                        moved.area.x0,
                        moved.area.y0,
                        moved.area.width(),
                        moved.area.height(),
                        moved.dx,
                        moved.dy,
                    ]);
                }
                let out = std::slice::from_raw_parts_mut(rects, max * 4);
                for (slot, rect) in out.chunks_exact_mut(4).zip(&list) {
                    slot.copy_from_slice(&[rect.x0, rect.y0, rect.width(), rect.height()]);
                }
                *count = list.len() as c_int;
            }
            2
        }
    }
}
