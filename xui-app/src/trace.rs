//! The trace sink of the std apps (docs/tracing-plan.md, T3): every evidence
//! line a harness reads off the serial port (`WRITER:SAVE:PASS`, `UI:RECT
//! ...`, `UI:STALL ...`) is built here by `libs/tracefmt` in one buffer and
//! written with one `write`, so another task's line can never land in the
//! middle (`tray.json` once did). It is the one place an xui app writes a
//! marker: a `println!("UP:...")` is a lint failure (`tools/trace/check_adhoc.py`).
//!
//! ```ignore
//! trace::pass!(Channel::Writer, "SAVE");                  // WRITER:SAVE:PASS
//! trace::pass!(Channel::Pdf, "OPEN", "{path}:{pages}");   // PDF:OPEN:PASS:/x.pdf:3
//! trace::fail!(Channel::Lrplay, "BIND", "{error}");       // LRPLAY:BIND:FAIL:code 2
//! trace::event!(Channel::Golf, "FPS", fps = fps, w = w);  // GOLF:FPS fps=.. w=..
//! trace::line!(Channel::Ui, "RECT x={x} y={y} name={name}");
//! if trace::enabled(Channel::Ui) { /* work that only feeds a trace */ }
//! ```
//!
//! With `cfg(lazyos_trace)` off (`LAZYOS_TRACE=0`) the macros expand to a dead
//! `if`: the arguments stay type-checked, nothing is evaluated. With it on,
//! the mask the kernel holds (`trace.mask=` in `lazyos.cfg`) is read once, on
//! the first line, through `system_stats` op `TRACE_MASK`.
//!
//! The line goes to `/dev/console` (the kernel terminal, serial included, and
//! in a `LAZYOS_DBGD=1` kernel the program-output ring `dbgctl log --source
//! programs` reads), even when the app's stdout is a Terminal window's pty.
//! Only a host build, where the device cannot be opened, falls back to stdout.
//!
//! A line that an operator needs on a PC with no serial port and no `dbgd` is
//! a class 2 diagnostic (the plan's section 3): [`print!`] writes it whatever
//! the switch and the mask say, as a command's result or a panic report.

use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

#[doc(hidden)]
pub use tracefmt as __fmt;
pub use tracefmt::{Budget, Channel};
use tracefmt::{Charge, LineBuf, Mask, Sink, Tracer, LINE_MAX};

/// Whether the trace macros are compiled in (`LAZYOS_TRACE`).
pub const ENABLED: bool = cfg!(lazyos_trace);

static TRACER: Tracer = Tracer::new();
static MASK_LOADED: AtomicBool = AtomicBool::new(false);

/// The console device, opened once; `None` when it cannot be (a host build,
/// and the host's own tests, which must not write a Linux machine's console).
fn console() -> Option<&'static File> {
    static CONSOLE: OnceLock<Option<File>> = OnceLock::new();
    CONSOLE
        .get_or_init(|| {
            if cfg!(test) {
                return None;
            }
            std::fs::OpenOptions::new()
                .write(true)
                .open(fhs::dev::CONSOLE)
                .ok()
        })
        .as_ref()
}

/// The terminal, one `write` per line: `writeln!` sends the text and the
/// newline separately, and another task's marker landing between the two
/// glued itself onto the line.
struct ConsoleSink;

impl Sink for ConsoleSink {
    fn write_line(&self, line: &[u8]) {
        let written = console().is_some_and(|mut device| device.write_all(line).is_ok());
        if !written {
            let _ = std::io::stdout().lock().write_all(line);
        }
    }
}

/// Take the kernel's mask the first time a channel is asked about. A kernel
/// that refuses the call keeps the default mask.
fn load_mask() {
    if MASK_LOADED.swap(true, Ordering::Relaxed) {
        return;
    }
    // The syscall is LazyOS's `int 0x80`: on a Linux host that is another call.
    if cfg!(test) {
        return;
    }
    if let Ok(words) = lazyos_sys::stats::trace_mask() {
        TRACER.set_mask(Mask::from_words(words));
    }
}

/// Whether `channel` prints right now: the way to skip work that only feeds a
/// trace (the shell measuring menu rows for `UI:RECT`).
pub fn enabled(channel: Channel) -> bool {
    if !ENABLED {
        return false;
    }
    load_mask();
    TRACER.enabled(channel)
}

/// The monotonic clock in the 10 ms ticks the channel budgets count in.
fn ticks() -> u64 {
    if cfg!(test) {
        return 0;
    }
    lazyos_sys::time::monotonic_ns() / 10_000_000
}

/// Build and write one line on `channel` (what the macros call).
pub fn emit(channel: Channel, build: impl FnOnce(&mut LineBuf<LINE_MAX>)) -> bool {
    if !enabled(channel) {
        return false;
    }
    TRACER.emit(&ConsoleSink, ticks(), channel, build)
}

/// Whether a line of one tag of `channel` may print under `budget`: for a
/// tag that can fire in a tight loop (`UI:STALL`) while the rest of its
/// channel is evidence that must never be dropped (`UI:RECT`). Prints the
/// budget's `SUPPRESSED` notice itself when a window opens after dropped
/// lines.
pub fn within(budget: &Budget, channel: Channel) -> bool {
    within_with(budget, channel, |count| {
        let mut notice = LineBuf::<96>::new();
        let _ = tracefmt::write::suppressed(&mut notice, channel, count);
        ConsoleSink.write_line(notice.finish());
    })
}

/// [`within`] with the notice left to the caller (a tag that has its own
/// notice text, `UI:STALL:SUPPRESSED n=.. app=..`).
pub fn within_with(budget: &Budget, channel: Channel, notice: impl FnOnce(u32)) -> bool {
    if !enabled(channel) {
        return false;
    }
    match budget.charge(ticks()) {
        Charge::Deny => false,
        Charge::Allow => true,
        Charge::AllowAfter(count) => {
            notice(count);
            true
        }
    }
}

/// Write one line whatever the mask and the switch say: a command's result
/// or a panic report, which is a product and not evidence about one. Same
/// line grammar and one write, so it cannot tear.
pub fn emit_always(build: impl FnOnce(&mut LineBuf<LINE_MAX>)) {
    TRACER.emit_always(&ConsoleSink, build);
}

/// `PREFIX:STAGE:PASS` or `PREFIX:STAGE:PASS:detail`.
#[macro_export]
#[doc(hidden)]
macro_rules! __trace_pass {
    ($channel:expr, $stage:expr) => {
        if $crate::trace::ENABLED {
            let channel = $channel;
            $crate::trace::emit(channel, |buf| {
                let _ = $crate::trace::__fmt::write::pass(buf, channel, $stage);
            });
        }
    };
    ($channel:expr, $stage:expr, $($detail:tt)+) => {
        if $crate::trace::ENABLED {
            let channel = $channel;
            $crate::trace::emit(channel, |buf| {
                let _ = $crate::trace::__fmt::write::pass_with(
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
                let _ = $crate::trace::__fmt::write::fail(
                    buf, channel, $stage, format_args!($($detail)+));
            });
        }
    };
}

/// `PREFIX:STAGE:WARN:detail`.
#[macro_export]
#[doc(hidden)]
macro_rules! __trace_warn {
    ($channel:expr, $stage:expr, $($detail:tt)+) => {
        if $crate::trace::ENABLED {
            let channel = $channel;
            $crate::trace::emit(channel, |buf| {
                let _ = $crate::trace::__fmt::write::warn(
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
                let _ = $crate::trace::__fmt::write::event(buf, channel, $name, &[
                    $((stringify!($key), &$value as &dyn ::core::fmt::Display)),*
                ]);
            });
        }
    };
}

/// `PREFIX:` and the formatted text, flattened to one line: for a line whose
/// shape is its own (`UI:RECT x=.. name=free text`).
#[macro_export]
#[doc(hidden)]
macro_rules! __trace_line {
    ($channel:expr, $($text:tt)+) => {
        if $crate::trace::ENABLED {
            let channel = $channel;
            $crate::trace::emit(channel, |buf| {
                let _ = $crate::trace::__fmt::write::line(
                    buf, channel, format_args!($($text)+));
            });
        }
    };
}

/// `PREFIX:` and the formatted text, flattened to one line, written even
/// with `LAZYOS_TRACE=0` or the channel masked: a command's result.
#[macro_export]
#[doc(hidden)]
macro_rules! __trace_print {
    ($channel:expr, $($text:tt)+) => {{
        let channel = $channel;
        $crate::trace::emit_always(|buf| {
            let _ = $crate::trace::__fmt::write::line(buf, channel, format_args!($($text)+));
        });
    }};
}

// Exported at the crate root under private names, re-exported here, so a
// program writes `trace::pass!` like the kernel's and the services'.
#[allow(unused_imports)] // the whole API; not every macro has a caller
pub use {
    __trace_event as event, __trace_fail as fail, __trace_line as line, __trace_pass as pass,
    __trace_print as print, __trace_warn as warn,
};

/// Markers of one program: a [`Channel`] bound once, so a binary with
/// dozens of stages names it in a single constant (`const MARK: Markers =
/// Markers::new(Channel::Lrplay)`).
#[derive(Clone, Copy, Debug)]
pub struct Markers {
    channel: Channel,
}

impl Markers {
    pub const fn new(channel: Channel) -> Markers {
        Markers { channel }
    }

    /// Prints `PREFIX:stage:PASS`.
    pub fn pass(&self, stage: &str) {
        pass!(self.channel, stage);
    }

    /// Prints a passing `stage` with a `detail` (a path, say), one line.
    pub fn pass_with(&self, stage: &str, detail: &str) {
        pass!(self.channel, stage, "{detail}");
    }

    /// Prints a failing `stage`; newlines in `detail` are flattened so a
    /// marker is always exactly one line.
    pub fn fail(&self, stage: &str, detail: &str) {
        fail!(self.channel, stage, "{detail}");
    }

    /// Prints a degraded `stage` on the console and on stderr (the Terminal
    /// window that started the program, when there is one).
    pub fn warn(&self, stage: &str, detail: &str) {
        warn!(self.channel, stage, "{detail}");
        // The same flattened line: a detail with a newline or an escape
        // sequence (a path, a document name) must not reach the window raw.
        let mut line = LineBuf::<LINE_MAX>::new();
        let _ = tracefmt::write::warn(&mut line, self.channel, stage, format_args!("{detail}"));
        let _ = std::io::stderr().write_all(line.finish());
    }

    /// A one-shot for `stage`: the returned closure prints the pass line the
    /// first time it is called and does nothing afterwards, so a session
    /// script can assert on it.
    pub fn once(&self, stage: &'static str) -> impl Fn() {
        let markers = *self;
        let fired = std::cell::Cell::new(false);
        move || {
            if !fired.replace(true) {
                markers.pass(stage);
            }
        }
    }

    /// Reports a panic as `PREFIX:PANIC:FAIL:<message>` and one
    /// `PREFIX:PANIC:BT:<frame>` per backtrace line on the console before
    /// the default hook runs. The program's stderr is usually a pipe to a
    /// Terminal window (or nowhere), so without this a crash leaves no serial
    /// evidence. A panic is a class 2 diagnostic: it prints whatever the
    /// switch and the mask say.
    pub fn install_panic_hook(self) {
        install_panic_hook(self.channel);
    }
}

/// [`Markers::install_panic_hook`] for a program that has no `Markers`.
pub fn install_panic_hook(channel: Channel) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        emit_always(|buf| {
            let _ = tracefmt::write::fail(buf, channel, "PANIC", format_args!("{info}"));
        });
        // Raw return addresses: the binary is static and not position
        // independent, so they symbolize offline against an unstripped build
        // (`CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_DEBUG=1`).
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        for frame in backtrace.lines() {
            emit_always(|buf| {
                let _ =
                    tracefmt::write::line(buf, channel, format_args!("PANIC:BT:{}", frame.trim()));
            });
        }
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn built(build: impl FnOnce(&mut LineBuf<LINE_MAX>)) -> String {
        let mut line = LineBuf::<LINE_MAX>::new();
        build(&mut line);
        String::from_utf8(line.finish().to_vec()).unwrap()
    }

    #[test]
    fn marker_lines_follow_the_documented_shape() {
        let channel = Channel::Lrplay;
        assert_eq!(
            built(|b| {
                let _ = tracefmt::write::pass(b, channel, "UP");
            }),
            "LRPLAY:UP:PASS\n"
        );
        assert_eq!(
            built(|b| {
                let _ = tracefmt::write::fail(b, Channel::Lride, "BIND", format_args!("code 2"));
            }),
            "LRIDE:BIND:FAIL:code 2\n"
        );
    }

    #[test]
    fn a_failure_detail_is_always_one_line() {
        let line = built(|b| {
            let _ = tracefmt::write::fail(b, Channel::Lrplay, "LOAD", format_args!("a\nb\r\nc"));
        });
        assert_eq!(line, "LRPLAY:LOAD:FAIL:a b  c\n");
        assert_eq!(line.matches('\n').count(), 1);
    }

    #[test]
    fn a_probe_rectangle_keeps_its_free_text_last_and_unquoted() {
        // The session scripts read `name=` to the end of the line.
        let line = built(|b| {
            let _ = tracefmt::write::line(
                b,
                Channel::Ui,
                format_args!(
                    "RECT x={} y={} w={} h={} name={}",
                    28, 500, 212, 24, "menu:Text Editor"
                ),
            );
        });
        assert_eq!(
            line,
            "UI:RECT x=28 y=500 w=212 h=24 name=menu:Text Editor\n"
        );
    }

    #[test]
    fn a_tag_budget_lets_its_lines_through_then_counts_the_rest() {
        if !ENABLED {
            return;
        }
        // One window (the test clock stays at tick 0): three lines, then
        // drops; the notice is for the next window and is not called here.
        let budget = Budget::new(3, 1000);
        let mut notices = 0;
        let allowed: Vec<bool> = (0..5)
            .map(|_| within_with(&budget, Channel::Ui, |_| notices += 1))
            .collect();
        assert_eq!(allowed, [true, true, true, false, false]);
        assert_eq!(notices, 0);
    }

    #[test]
    fn a_catalogued_channel_has_its_wire_prefix() {
        assert_eq!(Markers::new(Channel::Ui).channel.prefix(), "UI");
    }
}
