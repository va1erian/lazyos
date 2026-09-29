//! The taskbar clock (issue #370): date and time at the bar's right end.
//!
//! The instant comes straight from the kernel wall clock (UTC), so the clock
//! is right even when `timed` is not running. `timed` only supplies the time
//! zone: the compositor subscribes to its retained `time/tick` topic, looks
//! the zone up in the shared table and applies its offset itself, so a DST
//! change lands on the right minute instead of waiting for the next tick.
//!
//! The text lives in an inline buffer and is redrawn only when it changes
//! (once a minute): the user bump allocator never reclaims, so the loop must
//! not allocate per frame.

use alloc::vec::Vec;
use timezone::format::{format_clock, ClockText};
use timezone::Zone;
use user::central::{Bus, Subscription};
use user::messenger::display::{Face, Rect};
use user::messenger::{timed, DEFAULT_BUFFER, EXPIRED_DEADLINE};
use user::sys;

use super::theme::TASKBAR_H;

/// Ticks (100 Hz) between looks at the tick topic.
const POLL_TICKS: u64 = 100;
/// Ticks between attempts to reach the broker while it is unreachable.
const RETRY_TICKS: u64 = 500;
/// Padding either side of the clock text.
pub(super) const PAD: i32 = 12;
/// The widest line the format can produce, used to reserve the bar space so
/// taskbar entries never shift when the digits change.
const WIDEST: &str = "Wed 30 Sep 2026  00:00";

/// Width the bar reserves for the clock, on its right edge.
pub(super) fn reserved_width() -> i32 {
    Face::Serif.width(WIDEST) + PAD * 2
}

/// The clock's rectangle on a screen of `dims`.
pub(super) fn rect(dims: (i32, i32)) -> Rect {
    let width = reserved_width().min(dims.0);
    Rect::new(dims.0 - width, dims.1 - TASKBAR_H, width, TASKBAR_H)
}

pub(super) struct Clock {
    zone: &'static Zone,
    watch: Option<Subscription>,
    next_poll: u64,
    next_connect: u64,
    text: ClockText,
    /// Reused reply buffer for the topic poll.
    buffer: Vec<u8>,
}

impl Clock {
    pub(super) fn new() -> Clock {
        Clock {
            zone: timezone::default_zone(),
            watch: None,
            next_poll: 0,
            next_connect: 0,
            text: format_clock(0),
            buffer: alloc::vec![0u8; DEFAULT_BUFFER],
        }
    }

    /// The text to draw, e.g. `Tue 29 Sep 2026  21:04`.
    pub(super) fn text(&self) -> &str {
        self.text.as_str()
    }

    /// Refresh the zone and the text; `true` when the text changed and the
    /// clock rectangle needs a repaint.
    pub(super) fn poll(&mut self) -> bool {
        let now = sys::clock();
        if now >= self.next_poll {
            self.next_poll = now + POLL_TICKS;
            self.follow_zone(now);
        }
        let unix = (sys::wall_centis() / 100) as i64;
        let local = unix + i64::from(timezone::local(self.zone, unix).offset);
        let text = format_clock(local);
        let changed = text != self.text;
        self.text = text;
        changed
    }

    /// Connect to the broker if needed and apply any zone `timed` published.
    fn follow_zone(&mut self, now: u64) {
        if self.watch.is_none() {
            if now < self.next_connect {
                return;
            }
            self.next_connect = now + RETRY_TICKS;
            self.watch = Bus::connect()
                .and_then(|mut bus| bus.subscribe(timed::TICK_TOPIC))
                .ok();
        }
        let Some(watch) = &self.watch else {
            return;
        };
        loop {
            match watch.recv_with(&mut self.buffer, Some(EXPIRED_DEADLINE)) {
                Ok(Some(event)) => {
                    let zone = timed::decode_tick(&event.payload)
                        .ok()
                        .and_then(|tick| timezone::find(&tick.zone_name));
                    if let Some(zone) = zone {
                        self.zone = zone;
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    // The broker went away: resubscribe on a later poll.
                    self.watch = None;
                    break;
                }
            }
        }
    }
}
