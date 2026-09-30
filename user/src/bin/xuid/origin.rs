//! Open-origin hints (`HintOpenOrigin`, method 30): where the next window a
//! task creates should zoom open from (the folder tile that was just
//! double-clicked) instead of from its taskbar entry.
//!
//! A hint is untrusted input that may only steer one wireframe animation, so
//! it is validated and bounded here: it must come from the owner of the
//! surface it is relative to, is clamped to the screen, is dropped when empty
//! or off-screen, is kept one per task, and lapses after [`HINT_TTL_TICKS`] or
//! at the task's next `CreateSurface`. The rules are pure functions so the
//! boot self-test can exercise them.

use alloc::vec::Vec;
use libmessenger::Parcel;
use user::messenger::display::{wire, Rect};
use user::messenger::{self, Message};
use user::sys;

use super::compositor::Compositor;
use super::protocol::{empty_reply, error_reply};
use super::window::surface_by_id;

/// How long a hint stays valid, in PIT ticks (10 ms each): two seconds, far
/// longer than a double-click to `CreateSurface` round trip needs.
pub(super) const HINT_TTL_TICKS: u64 = 200;

/// One pending hint: the screen rectangle to start from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct OpenHint {
    /// The task that gave it (kernel-stamped sender slot).
    pub(super) owner: u64,
    /// The start rectangle, already screen-absolute and clamped.
    pub(super) from: Rect,
    /// The tick after which the hint is stale.
    pub(super) expires: u64,
}

/// The screen rectangle for a hint `(x, y, w, h)` relative to `content`'s
/// origin, clamped to `screen`; `None` when it is empty or entirely off the
/// screen. Arithmetic is 64-bit so hostile values cannot wrap.
pub(super) fn translate(content: Rect, hint: (i32, i32, u32, u32), screen: Rect) -> Option<Rect> {
    let (x, y, w, h) = hint;
    if w == 0 || h == 0 {
        return None;
    }
    let left = (content.x as i64 + x as i64).max(screen.x as i64);
    let top = (content.y as i64 + y as i64).max(screen.y as i64);
    let right = (content.x as i64 + x as i64 + w as i64).min((screen.x + screen.w) as i64);
    let bottom = (content.y as i64 + y as i64 + h as i64).min((screen.y + screen.h) as i64);
    if right <= left || bottom <= top {
        return None;
    }
    Some(Rect::new(
        left as i32,
        top as i32,
        (right - left) as i32,
        (bottom - top) as i32,
    ))
}

/// Record `hint`, replacing any earlier one from the same task and dropping
/// stale ones, so the list never outgrows the number of live tasks.
pub(super) fn store(hints: &mut Vec<OpenHint>, hint: OpenHint, now: u64) {
    hints.retain(|old| old.owner != hint.owner && old.expires > now);
    hints.push(hint);
}

/// Consume `owner`'s hint, if it has one that has not expired.
pub(super) fn take(hints: &mut Vec<OpenHint>, owner: u64, now: u64) -> Option<Rect> {
    let index = hints.iter().position(|hint| hint.owner == owner)?;
    let hint = hints.swap_remove(index);
    (hint.expires > now).then_some(hint.from)
}

impl Compositor {
    /// `HintOpenOrigin`: remember where `message.sender`'s next window opens
    /// from. Only the owner of `surface` may hint relative to it.
    pub(super) fn hint_open_origin(&mut self, message: &Message, body: &[u8]) -> Parcel {
        let Ok(args) = wire::decode_hint_open_origin_args(body) else {
            return error_reply(message.method(), messenger::errno::EINVAL);
        };
        let Some(surface) = surface_by_id(&self.surfaces, args.surface) else {
            return error_reply(message.method(), messenger::errno::ENOENT);
        };
        if surface.owner != message.sender {
            return error_reply(message.method(), messenger::errno::EACCES);
        }
        // A minimized window has no on-screen content to start from, and the
        // desktop layer has no window to open "from".
        if surface.minimized || surface.desktop {
            return empty_reply(message.method());
        }
        if let Some(from) = translate(
            surface.content(),
            (args.x, args.y, args.w, args.h),
            self.full(),
        ) {
            let now = sys::clock();
            let hint = OpenHint {
                owner: message.sender,
                from,
                expires: now + HINT_TTL_TICKS,
            };
            store(&mut self.hints, hint, now);
        }
        empty_reply(message.method())
    }
}

/// Boot check of the hint rules: `XUID:ORIGIN:PASS` or `XUID:ORIGIN:FAIL`.
pub(super) fn selftest_open_origin() -> &'static str {
    let screen = Rect::new(0, 0, 800, 600);
    let content = Rect::new(100, 50, 400, 300);
    // Content-relative rect becomes screen-absolute.
    let plain = translate(content, (10, 20, 48, 48), screen) == Some(Rect::new(110, 70, 48, 48));
    // Clamped to the screen edges, and dropped when wholly off or empty.
    let clamped =
        translate(content, (-200, -100, 200, 100), screen) == Some(Rect::new(0, 0, 100, 50));
    let off = translate(content, (900, 0, 10, 10), screen).is_none()
        && translate(content, (-500, 0, 10, 10), screen).is_none();
    let empty = translate(content, (0, 0, 0, 5), screen).is_none()
        && translate(content, (0, 0, 5, 0), screen).is_none();
    // Hostile extremes neither wrap nor panic.
    let huge = translate(content, (i32::MIN, i32::MAX, u32::MAX, u32::MAX), screen).is_none()
        && translate(content, (i32::MAX, i32::MIN, u32::MAX, u32::MAX), screen).is_none()
        && translate(content, (i32::MIN, i32::MIN, u32::MAX, u32::MAX), screen) == Some(screen);

    let rect = Rect::new(1, 2, 3, 4);
    let mut hints = Vec::new();
    store(
        &mut hints,
        OpenHint {
            owner: 7,
            from: rect,
            expires: 100,
        },
        0,
    );
    // One per task: a second hint replaces the first.
    let other = Rect::new(5, 6, 7, 8);
    store(
        &mut hints,
        OpenHint {
            owner: 7,
            from: other,
            expires: 100,
        },
        1,
    );
    store(
        &mut hints,
        OpenHint {
            owner: 9,
            from: rect,
            expires: 100,
        },
        1,
    );
    let single = hints.len() == 2;
    // Taking consumes it, and only for its owner.
    let took = take(&mut hints, 7, 2) == Some(other) && take(&mut hints, 7, 2).is_none();
    // An expired hint is dropped, not returned.
    let expired = take(&mut hints, 9, 100).is_none() && hints.is_empty();
    // Storing prunes other tasks' stale hints.
    store(
        &mut hints,
        OpenHint {
            owner: 1,
            from: rect,
            expires: 10,
        },
        0,
    );
    store(
        &mut hints,
        OpenHint {
            owner: 2,
            from: rect,
            expires: 50,
        },
        20,
    );
    let pruned = hints.len() == 1 && hints[0].owner == 2;

    if plain && clamped && off && empty && huge && single && took && expired && pruned {
        "XUID:ORIGIN:PASS\n"
    } else {
        "XUID:ORIGIN:FAIL\n"
    }
}
