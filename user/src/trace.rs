//! The trace sink of the no_std services (docs/tracing-plan.md, T2): every
//! evidence line a harness reads off the serial port (`INIT:AUTOSTART:PASS`,
//! `USBD:DIAG ...`) is built here by `libs/tracefmt` in one buffer and written
//! with one `write`, so another task's line can never land in the middle.
//!
//! ```ignore
//! trace::pass!(Channel::Pkgd, "BOOT");                    // PKGD:BOOT:PASS
//! trace::fail!(Channel::Pkgd, "BOOT", "{error}");         // PKGD:BOOT:FAIL:..
//! trace::event!(Channel::Usbd, "DIAG", slot = slot);      // USBD:DIAG slot=..
//! trace::line!(Channel::Init, "STOP:{name}:DONE");        // INIT:STOP:..
//! ```
//!
//! With `cfg(lazyos_trace)` off (`LAZYOS_TRACE=0`) the macros expand to a dead
//! `if`: the arguments stay type-checked, nothing is evaluated. With it on,
//! the mask the kernel holds (`trace.mask=` in `lazyos.cfg`) is read once, on
//! the first line, through `system_stats` op `TRACE_MASK`, and a channel with
//! a rate limit is charged per process.
//!
//! The line goes to the terminal write, which the kernel mirrors to the serial
//! port and, in a `LAZYOS_DBGD=1` kernel, to the program-output ring.

use core::sync::atomic::{AtomicBool, Ordering};

pub use tracefmt::Channel;
use tracefmt::{LineBuf, Mask, Sink, Tracer, LINE_MAX};

/// Whether the trace macros are compiled in (`LAZYOS_TRACE`).
pub const ENABLED: bool = cfg!(lazyos_trace);

static TRACER: Tracer = Tracer::new();
static MASK_LOADED: AtomicBool = AtomicBool::new(false);

/// The terminal, one `write` per line.
struct ConsoleSink;

impl Sink for ConsoleSink {
    fn write_line(&self, line: &[u8]) {
        crate::sys::write(line);
    }
}

/// Take the kernel's mask the first time a channel is asked about. A kernel
/// that refuses the call keeps the default mask.
fn load_mask() {
    if MASK_LOADED.swap(true, Ordering::Relaxed) {
        return;
    }
    if let Ok(words) = crate::sys::trace_mask() {
        TRACER.set_mask(Mask::from_words(words));
    }
}

/// Whether `channel` prints right now: the way to skip work that only feeds a
/// trace.
pub fn enabled(channel: Channel) -> bool {
    if !ENABLED {
        return false;
    }
    load_mask();
    TRACER.enabled(channel)
}

/// Build and write one line on `channel` (what the macros call). The
/// channel's rate limit counts in 10 ms ticks of the monotonic clock.
pub fn emit(channel: Channel, build: impl FnOnce(&mut LineBuf<LINE_MAX>)) -> bool {
    if !ENABLED {
        return false;
    }
    load_mask();
    TRACER.emit(
        &ConsoleSink,
        crate::sys::monotonic_ms() / 10,
        channel,
        build,
    )
}

/// Whether a line of one tag of `channel` may print under `budget`: for a
/// tag that can fire in a tight loop (`USBD:DIAG`) while the rest of its
/// channel is evidence that must never be dropped. Prints the budget's
/// `SUPPRESSED` notice when a window opens after dropped lines.
pub fn within(budget: &tracefmt::Budget, channel: Channel) -> bool {
    if !enabled(channel) {
        return false;
    }
    match budget.charge(crate::sys::monotonic_ms() / 10) {
        tracefmt::Charge::Deny => false,
        tracefmt::Charge::Allow => true,
        tracefmt::Charge::AllowAfter(count) => {
            let mut notice = LineBuf::<96>::new();
            let _ = tracefmt::write::suppressed(&mut notice, channel, count);
            ConsoleSink.write_line(notice.finish());
            true
        }
    }
}

/// `PREFIX:STAGE:PASS` or `PREFIX:STAGE:PASS:detail`.
#[macro_export]
#[doc(hidden)]
macro_rules! __trace_pass {
    ($channel:expr, $stage:expr) => {
        if $crate::trace::ENABLED {
            let channel = $channel;
            $crate::trace::emit(channel, |buf| {
                let _ = ::tracefmt::write::pass(buf, channel, $stage);
            });
        }
    };
    ($channel:expr, $stage:expr, $($detail:tt)+) => {
        if $crate::trace::ENABLED {
            let channel = $channel;
            $crate::trace::emit(channel, |buf| {
                let _ = ::tracefmt::write::pass_with(
                    buf, channel, $stage, format_args!($($detail)+));
            });
        }
    };
}

/// `PREFIX:STAGE:FAIL:detail`.
#[macro_export]
#[doc(hidden)]
macro_rules! __trace_fail {
    ($channel:expr, $stage:expr, $($detail:tt)+) => {
        if $crate::trace::ENABLED {
            let channel = $channel;
            $crate::trace::emit(channel, |buf| {
                let _ = ::tracefmt::write::fail(
                    buf, channel, $stage, format_args!($($detail)+));
            });
        }
    };
}

/// `PREFIX:NAME key=value key="a b"`.
#[macro_export]
#[doc(hidden)]
macro_rules! __trace_event {
    ($channel:expr, $name:expr $(, $key:ident = $value:expr)* $(,)?) => {
        if $crate::trace::ENABLED {
            let channel = $channel;
            $crate::trace::emit(channel, |buf| {
                let _ = ::tracefmt::write::event(buf, channel, $name, &[
                    $((stringify!($key), &$value as &dyn ::core::fmt::Display)),*
                ]);
            });
        }
    };
}

/// `PREFIX:` and the formatted text, flattened to one line.
#[macro_export]
#[doc(hidden)]
macro_rules! __trace_line {
    ($channel:expr, $($text:tt)+) => {
        if $crate::trace::ENABLED {
            let channel = $channel;
            $crate::trace::emit(channel, |buf| {
                let _ = ::tracefmt::write::line(buf, channel, format_args!($($text)+));
            });
        }
    };
}

// Exported at the crate root under private names, re-exported here, so a
// program writes `user::trace::pass!` like the kernel's `trace::pass!`.
#[allow(unused_imports)] // the whole API; not every macro has a caller
pub use {
    __trace_event as event, __trace_fail as fail, __trace_line as line, __trace_pass as pass,
};
