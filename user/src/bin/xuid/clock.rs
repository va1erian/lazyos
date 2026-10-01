//! The taskbar clock (issue #370): date and time at the bar's right end.
//!
//! The instant comes straight from the kernel wall clock (UTC), so the clock
//! is right even when `timed` is not running. `timed` only supplies the time
//! zone: the compositor subscribes to its retained `time/tick` topic, looks
//! the zone up in the shared table and applies its offset itself, so a DST
//! change lands on the right minute instead of waiting for the next tick.
//!
//! The format (12/24-hour, seconds) comes from `confd` through
//! [`themefeed`](super::themefeed), which calls [`set_format`].
//!
//! The text lives in an inline buffer and is redrawn only when it changes
//! (once a minute, or once a second with seconds shown): the user bump
//! allocator never reclaims, so the loop must not allocate per frame.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU8, Ordering};
use timezone::format::{format_clock, format_clock_as, ClockFormat, ClockText};
use timezone::Zone;
use user::central::{Bus, Subscription};
use user::messenger::display::{Face, Rect};
use user::messenger::topics_client::Qos;
use user::messenger::{timed, DEFAULT_BUFFER, EXPIRED_DEADLINE};
use user::sys;

use super::theme::TASKBAR_H;

/// Ticks (100 Hz) between looks at the tick topic.
const POLL_TICKS: u64 = 100;
/// Ticks between attempts to reach the broker while it is unreachable.
const RETRY_TICKS: u64 = 500;
/// Ticks the subscribe call may wait for the broker before it is abandoned.
const SUBSCRIBE_TICKS: u64 = 5;
/// Padding either side of the clock text.
pub(super) const PAD: i32 = 12;
/// [`FORMAT`] bit: 24-hour time.
const HOUR24: u8 = 1;
/// [`FORMAT`] bit: seconds shown.
const SECONDS: u8 = 2;

/// The clock format in effect. A static rather than a [`Clock`] field because
/// the taskbar layout ([`reserved_width`]) is computed by free functions;
/// `xuid` is a single task, so relaxed ordering is enough.
static FORMAT: AtomicU8 = AtomicU8::new(HOUR24);

fn format() -> ClockFormat {
    let bits = FORMAT.load(Ordering::Relaxed);
    ClockFormat {
        hour24: bits & HOUR24 != 0,
        seconds: bits & SECONDS != 0,
    }
}

/// Install `format`; `true` when it changed.
pub(super) fn set_format(format: ClockFormat) -> bool {
    let bits = if format.hour24 { HOUR24 } else { 0 } | if format.seconds { SECONDS } else { 0 };
    FORMAT.swap(bits, Ordering::Relaxed) != bits
}

/// Width the bar reserves for the clock, on its right edge: the widest line
/// the current format can produce, so taskbar entries never shift when the
/// digits change.
pub(super) fn reserved_width() -> i32 {
    Face::Serif.width(format().widest()) + PAD * 2
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

    /// The text to draw, e.g. `Tue 29 Sep 2026  21:04` or `9:04:59 PM`.
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
        let text = format_clock_as(local, format());
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
            // Bounded: the compositor must not stall on a silent broker.
            let deadline = Some(now + SUBSCRIBE_TICKS);
            self.watch = Bus::connect()
                .and_then(|mut bus| {
                    bus.subscribe_with_deadline(timed::TICK_TOPIC, Qos::Latest, deadline)
                })
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
