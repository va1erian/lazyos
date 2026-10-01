//! The shutting-down screen (docs/shutdown.md, stage S-c): `xuid` follows
//! `init`'s retained `system/power/state` topic and, from the first phase on,
//! paints a full-screen overlay ("Shutting down..." or "Restarting...") over
//! everything. The kernel spawned `xuid`, not `init`, so it stays up until the
//! machine stops and the overlay is the last frame on screen.
//!
//! The feed subscribes on `init`'s own topic broker (not `messengerd`'s, which
//! `init` stops before the end), retrying quietly while `init` is not there.
//! The overlay state is two atomics, like the menu's, so [`draw`] needs no
//! compositor state.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use alloc::vec::Vec;
use user::messenger::display::{Canvas, Face, Rect};
use user::messenger::{router, services, DEFAULT_BUFFER, EXPIRED_DEADLINE};
use user::sys;

use super::theme::{overlay_bg, overlay_border, overlay_text};

/// Ticks (100 Hz) between looks at the topic.
const POLL_TICKS: u64 = 10;
/// Ticks between attempts to subscribe while `init` is unreachable.
const RETRY_TICKS: u64 = 300;
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
pub(super) fn show(mode: u32) -> bool {
    MODE.store(mode, Ordering::Relaxed);
    let raised = !ACTIVE.swap(true, Ordering::Relaxed);
    if raised {
        sys::write_str("XUID:POWER:OVERLAY\n");
    }
    raised
}

/// Take the overlay down (the request it was raised for was refused).
pub(super) fn hide() {
    ACTIVE.store(false, Ordering::Relaxed);
}

/// Follows `system/power/state` on `init`'s broker.
pub(super) struct PowerFeed {
    sub: Option<router::Subscriber>,
    next_poll: u64,
    next_connect: u64,
    buffer: Vec<u8>,
}

impl PowerFeed {
    pub(super) fn new() -> PowerFeed {
        PowerFeed {
            sub: None,
            next_poll: 0,
            next_connect: 0,
            buffer: alloc::vec![0u8; DEFAULT_BUFFER],
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
        if self.sub.is_none() {
            if now < self.next_connect {
                return false;
            }
            self.next_connect = now + RETRY_TICKS;
            self.sub = router::Bus::connect(services::INIT_NAME)
                .and_then(|mut bus| services::init::wire::subscribe_system_power_state(&mut bus))
                .ok();
        }
        let mut raised = false;
        while let Some(sub) = &self.sub {
            match sub.recv_with(&mut self.buffer, Some(EXPIRED_DEADLINE)) {
                Ok(Some(event)) => {
                    if let Ok(state) =
                        services::init::wire::decode_system_power_state(&event.payload)
                    {
                        raised |= show(state.mode);
                    }
                }
                Ok(None) => break,
                // The broker went away (it is `init`; only a crash does
                // that): subscribe again later.
                Err(_) => self.sub = None,
            }
        }
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
    let frame = Rect::new((w - BOX_W) / 2, (h - BOX_H) / 2, BOX_W, BOX_H);
    screen.fill(frame, clip, overlay_border());
    screen.fill(
        Rect::new(frame.x + 1, frame.y + 1, frame.w - 2, frame.h - 2),
        clip,
        overlay_bg(),
    );
    // Two lines, the free height split evenly above, between and below them.
    let line_h = Face::Sans.height();
    let gap = (frame.h - 2 * line_h) / 3;
    for (row, text) in [message, detail].into_iter().enumerate() {
        let x = frame.x + (frame.w - Face::Sans.width(text)) / 2;
        let y = frame.y + gap + (gap + line_h) * row as i32;
        screen.text_face(x, y, text, Face::Sans, overlay_text(), frame.intersect(clip));
    }
}
