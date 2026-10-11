//! The kernel's trace sink (docs/tracing-plan.md, T1): every evidence line a
//! test or a person reads off the serial port (`DEV:`, `BOOT:PHASE`,
//! `LABEL:DENY`, `PERF:`, `TEST:`) is built here, in one buffer, by
//! `libs/tracefmt`, and written with one call, so another task's line can
//! never land in the middle of it.
//!
//! ```ignore
//! trace::pass!(Channel::Dev, "ENUM");                     // DEV:ENUM:PASS
//! trace::fail!(Channel::Dev, "ENUM", "{n} devices", n = 0);
//! trace::event!(Channel::Label, "DENY", label = name);    // LABEL:DENY label=..
//! trace::line!(Channel::Boot, "PHASE:{name}:tsc={tsc}");  // BOOT:PHASE:..
//! ```
//!
//! With `cfg(lazyos_trace)` off (`LAZYOS_TRACE=0`) the macros expand to a
//! dead `if`: the arguments stay type-checked, nothing is evaluated and the
//! optimizer drops it. With it on, the channel's bit in the mask
//! (`trace.mask=` in `lazyos.cfg`, read where the `limit.*` lines are) is
//! checked first and a channel with a rate limit is charged.
//!
//! Where a trace goes: serial, and in a `LAZYOS_DBGD=1` kernel the
//! program-output ring that `dbgd` serves as `log.tail source=programs`. It
//! is *not* written to the boot log ring (`klog`): that holds the diagnostics
//! (`HW:`, `EXCEPTION:`, `limits:`) a chatty trace must not evict.
//! `TEST:` lines (the kernel suite's protocol) take the same path but ignore
//! the mask and the switch: a test image reports whether tracing is on or not.

use tracefmt::{Channel, LineBuf, Mask, Sink, Tracer, LINE_MAX};

/// Whether the trace macros are compiled in (`LAZYOS_TRACE`).
pub const ENABLED: bool = cfg!(lazyos_trace);

static TRACER: Tracer = Tracer::new();

/// The serial port (and, in a `dbgd` kernel, the program-output ring).
struct KernelSink;

impl Sink for KernelSink {
    fn write_line(&self, line: &[u8]) {
        #[cfg(lazyos_tests)]
        if let Some(hook) = capture::hook() {
            hook(line);
            return;
        }
        crate::serial::write_trace(line);
    }
}

/// Test hook: divert finished lines to a function instead of the serial
/// port (a million lines would take minutes at 38400 baud, and a test must
/// look at what was written).
#[cfg(lazyos_tests)]
pub mod capture {
    use core::sync::atomic::{AtomicPtr, Ordering};

    static HOOK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

    pub fn hook() -> Option<fn(&[u8])> {
        let raw = HOOK.load(Ordering::Acquire);
        if raw.is_null() {
            return None;
        }
        // SAFETY: only `set` stores a non-null value, and it stores a
        // `fn(&[u8])` cast to a data pointer.
        Some(unsafe { core::mem::transmute::<*mut (), fn(&[u8])>(raw) })
    }

    /// Divert lines to `hook` until [`clear`].
    pub fn set(hook: fn(&[u8])) {
        HOOK.store(hook as *mut (), Ordering::Release);
    }

    pub fn clear() {
        HOOK.store(core::ptr::null_mut(), Ordering::Release);
    }
}

/// Whether `channel` prints right now.
pub fn enabled(channel: Channel) -> bool {
    ENABLED && TRACER.enabled(channel)
}

/// Build and write one line on `channel` (what the macros call). The
/// channel's rate limit counts in 10 ms timer ticks.
pub fn emit(channel: Channel, build: impl FnOnce(&mut LineBuf<LINE_MAX>)) -> bool {
    ENABLED && TRACER.emit(&KernelSink, crate::task::ticks(), channel, build)
}

/// Write `line`, a complete line that already starts with the channel's
/// prefix, of whatever length (a report that is longer than a trace buffer).
/// The mask and the rate limit apply as for [`emit`].
#[allow(dead_code)] // the perf report (`LAZYOS_PERF=1`)
pub fn emit_str(channel: Channel, line: &str) -> bool {
    if !ENABLED || !TRACER.permit(&KernelSink, crate::task::ticks(), channel) {
        return false;
    }
    debug_assert!(line.starts_with(channel.prefix()));
    let line = line.strip_suffix('\n').unwrap_or(line);
    // One write, newline included, so the line cannot be torn.
    let mut whole = alloc::string::String::with_capacity(line.len() + 1);
    whole.push_str(line);
    whole.push('\n');
    KernelSink.write_line(whole.as_bytes());
    true
}

/// Build and write one `TEST:` line, whatever the mask and the switch say.
#[allow(dead_code)] // the kernel suite (`LAZYOS_TESTS=1`)
pub fn emit_test(build: impl FnOnce(&mut LineBuf<LINE_MAX>)) {
    TRACER.emit_always(&KernelSink, build);
}

/// The mask in force (empty when the macros are compiled out), as two words.
pub fn mask_words() -> [u64; 2] {
    if ENABLED {
        TRACER.mask().words()
    } else {
        Mask::NONE.words()
    }
}

/// Apply the `trace.mask=` line of a `lazyos.cfg` text. Like the limits
/// the file is untrusted input: every problem is logged, none is fatal, a
/// later line replaces an earlier one.
pub fn apply_config(text: &str) {
    for line in text.lines() {
        let Some(value) = line.trim().strip_prefix("trace.mask=") else {
            continue;
        };
        let mask = Mask::parse(value, |problem| match problem {
            tracefmt::Problem::Unknown(name) => {
                serial_println!("trace: unknown channel {name} ignored")
            }
            tracefmt::Problem::Empty => serial_println!("trace: empty entry in trace.mask ignored"),
        });
        TRACER.set_mask(mask);
    }
}

/// Test hook: [`emit`] at a chosen time on the budget's clock.
#[cfg(lazyos_tests)]
pub fn emit_at(now: u64, channel: Channel, build: impl FnOnce(&mut LineBuf<LINE_MAX>)) -> bool {
    TRACER.emit(&KernelSink, now, channel, build)
}

/// Test hook: put the mask and the budgets back to the default.
#[cfg(lazyos_tests)]
pub fn reset_for_test() {
    TRACER.set_mask(Mask::defaults());
    TRACER.reset_budgets();
}

/// Test hook: set the mask directly.
#[cfg(lazyos_tests)]
pub fn set_mask_for_test(mask: Mask) {
    TRACER.set_mask(mask);
}

/// Test hook: the mask that is set, whether or not the macros are compiled in.
#[cfg(lazyos_tests)]
pub fn live_mask_for_test() -> Mask {
    TRACER.mask()
}

/// `PREFIX:STAGE:PASS` or `PREFIX:STAGE:PASS:detail`.
#[allow(unused_macros)] // migrated call sites come with later stages
macro_rules! pass {
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
macro_rules! fail {
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
macro_rules! event {
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
macro_rules! line {
    ($channel:expr, $($text:tt)+) => {
        if $crate::trace::ENABLED {
            let channel = $channel;
            $crate::trace::emit(channel, |buf| {
                let _ = ::tracefmt::write::line(buf, channel, format_args!($($text)+));
            });
        }
    };
}

/// `TEST:` and the formatted text: the kernel suite's protocol
/// (`TEST:<name>:PASS|FAIL|INFO:<detail>`), written whatever the mask says.
#[allow(unused_macros)] // the kernel suite only
macro_rules! test {
    ($($text:tt)+) => {
        $crate::trace::emit_test(|buf| {
            let _ = ::tracefmt::write::line(
                buf, ::tracefmt::Channel::Test, format_args!($($text)+));
        })
    };
}

// The whole set is the API later stages migrate onto; not every macro has a
// caller yet.
#[allow(unused_imports)]
pub(crate) use {event, fail, line, pass, test};
