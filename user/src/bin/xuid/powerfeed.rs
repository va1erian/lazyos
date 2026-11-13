//! The shutting-down screen (docs/shutdown.md, stage S-c): `xuid` follows
//! `init`'s retained `system/power/state` topic and, from the first phase on,
//! paints a full-screen overlay ("Shutting down..." or "Restarting...") over
//! everything. The kernel spawned `xuid`, not `init`, so it stays up until the
//! machine stops and the overlay is the last frame on screen.
//!
//! The feed follows the topic on the central broker through a doorbell it
//! asks locally (never the broker), retrying quietly while `messengerd` is
//! not there. `init` stops `messengerd` before the end, but publishes its
//! first phase before it stops anything, which is all the overlay needs.
//! The overlay state is two atomics, so [`draw`] needs no compositor state.
//! Nothing raises it but the topic: the power rows live in LazyShell's start
//! menu (`xui-app/src/shell/power.rs`), which asks `init` for the shutdown,
//! and `init` publishes its first phase before it stops anything.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use user::trace;

use user::central::Feed;
use user::messenger::display::{Canvas, Face, Rect};
use user::messenger::services;
use user::sys;

use super::theme::{overlay_bg, overlay_border, overlay_text, px};

/// Ticks (100 Hz) between looks at the doorbell.
const POLL_TICKS: u64 = 10;
/// The overlay's message box.
const BOX_W: i32 = 320;
const BOX_H: i32 = 72;

static ACTIVE: AtomicBool = AtomicBool::new(false);
/// The `PowerMode` being performed.
static MODE: AtomicU32 = AtomicU32::new(0);

/// Whether the shutting-down overlay is up (input is then ignored).
pub(super) fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Raise the overlay for `mode` (a `PowerMode`); `true` when it was not up.
fn show(mode: u32) -> bool {
    MODE.store(mode, Ordering::Relaxed);
    let raised = !ACTIVE.swap(true, Ordering::Relaxed);
    if raised {
        trace::line!(trace::Channel::Xuid, "POWER:OVERLAY");
    }
    raised
}

/// Follows `system/power/state`.
pub(super) struct PowerFeed {
    feed: Feed<services::PowerState>,
    next_poll: u64,
}

impl PowerFeed {
    pub(super) fn new() -> PowerFeed {
        PowerFeed {
            feed: Feed::new(services::init::wire::subscribe_system_power_state),
            next_poll: 0,
        }
    }

    /// Take in any power state; `true` when the overlay just went up and the
    /// screen needs a full repaint.
    pub(super) fn poll(&mut self) -> bool {
        let now = sys::clock();
        if now < self.next_poll {
            return false;
        }
        self.next_poll = now + POLL_TICKS;
        self.feed.ensure();
        let mut raised = false;
        self.feed
            .drain_rung(|event| raised |= show(event.value.mode));
        raised
    }
}

/// Paint the overlay over `clip` (no-op when it is down): the whole screen
/// dimmed to the overlay colour and a centred box with the message.
pub(super) fn draw(screen: &mut Canvas, clip: Rect) {
    if !active() {
        return;
    }
    let (w, h) = (screen.width(), screen.height());
    screen.fill(Rect::new(0, 0, w, h), clip, overlay_bg());
    let message = if MODE.load(Ordering::Relaxed) == services::POWER_MODE_REBOOT {
        "Restarting..."
    } else {
        "Shutting down..."
    };
    let detail = "Saving settings and stopping services";
    let (box_w, box_h) = (px(BOX_W), px(BOX_H));
    let frame = Rect::new((w - box_w) / 2, (h - box_h) / 2, box_w, box_h);
    screen.fill(frame, clip, overlay_border());
    screen.fill(
        Rect::new(
            frame.x + px(1),
            frame.y + px(1),
            frame.w - px(2),
            frame.h - px(2),
        ),
        clip,
        overlay_bg(),
    );
    // Two lines, the free height split evenly above, between and below them.
    let line_h = Face::Sans.height();
    let gap = (frame.h - 2 * line_h) / 3;
    for (row, text) in [message, detail].into_iter().enumerate() {
        let x = frame.x + (frame.w - Face::Sans.width(text)) / 2;
        let y = frame.y + gap + (gap + line_h) * row as i32;
        screen.text_face(
            x,
            y,
            text,
            Face::Sans,
            overlay_text(),
            frame.intersect(clip),
        );
    }
}
