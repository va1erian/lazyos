//! The trace sink (`trace.rs`, docs/tracing-plan.md T1): line shapes from the
//! kernel side, the mask read from `lazyos.cfg`, a masked channel writing
//! nothing, the budget's `SUPPRESSED` line, and soaks (two kernel threads
//! racing lines through the sink, a million masked lines in bounded time).
//!
//! Lines are diverted to a hook (`trace::capture`) instead of COM1, so a
//! soak does not wait on 38400 baud and the test can look at the bytes.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use super::*;
use crate::task::PriorityClass;
use crate::trace::{self, capture};
use tracefmt::{Channel, Mask};

static LINES: spin::Mutex<Vec<u8>> = spin::Mutex::new(Vec::new());

fn store(line: &[u8]) {
    x86_64::instructions::interrupts::without_interrupts(|| LINES.lock().extend_from_slice(line));
}

/// Run `body` with lines captured under `mask`; returns what was written.
fn captured(mask: &str, body: impl FnOnce()) -> String {
    x86_64::instructions::interrupts::without_interrupts(|| LINES.lock().clear());
    trace::set_mask_for_test(Mask::parse(mask, |_| {}));
    capture::set(store);
    body();
    capture::clear();
    trace::reset_for_test();
    let bytes = x86_64::instructions::interrupts::without_interrupts(|| LINES.lock().clone());
    String::from_utf8(bytes).unwrap_or_default()
}

fn line_shapes() -> Result<(), String> {
    let text = captured("all", || {
        trace::pass!(Channel::Dev, "ENUM");
        trace::pass!(Channel::Dev, "IRQ", "{} vectors", 16);
        trace::fail!(Channel::Fs, "MOUNT", "no {}", "root");
        trace::event!(
            Channel::Label,
            "DENY",
            label = "calc",
            iface = 7,
            note = "a b"
        );
        trace::line!(Channel::Boot, "PHASE:{}:tsc={}", "mem_ready", 99);
    });
    if !trace::ENABLED {
        check!(text.is_empty(), "compiled out, yet wrote {text:?}");
        return Ok(());
    }
    let want = "DEV:ENUM:PASS\nDEV:IRQ:PASS:16 vectors\nFS:MOUNT:FAIL:no root\n\
                LABEL:DENY label=calc iface=7 note=\"a b\"\nBOOT:PHASE:mem_ready:tsc=99\n";
    check!(text == want, "got {text:?}");
    Ok(())
}

fn details_stay_on_one_line() -> Result<(), String> {
    if !trace::ENABLED {
        return Ok(());
    }
    let text = captured("all", || {
        trace::fail!(Channel::Dev, "X", "a\nb\r\nc");
    });
    check!(text == "DEV:X:FAIL:a b  c\n", "got {text:?}");
    Ok(())
}

fn mask_comes_from_the_config() -> Result<(), String> {
    if !trace::ENABLED {
        check!(trace::mask_words() == [0, 0], "compiled out, yet a mask");
        return Ok(());
    }
    trace::apply_config("limit.fd_max=100\ntrace.mask=init,pkgd\n");
    let first = (
        trace::enabled(Channel::Init),
        trace::enabled(Channel::Pkgd),
        trace::enabled(Channel::Shell),
    );
    // A later line replaces an earlier one; an unknown channel is skipped.
    trace::apply_config("trace.mask=-shell,nosuch\n");
    let second = (
        trace::enabled(Channel::Init),
        trace::enabled(Channel::Shell),
        trace::enabled(Channel::Label),
    );
    trace::reset_for_test();
    check!(first == (true, true, false), "list: {first:?}");
    check!(second == (true, false, false), "removal: {second:?}");
    Ok(())
}

fn a_masked_channel_writes_nothing() -> Result<(), String> {
    static EVALUATED: AtomicBool = AtomicBool::new(false);
    fn touch() -> u32 {
        EVALUATED.store(true, Ordering::Relaxed);
        1
    }
    let text = captured("init", || {
        trace::pass!(Channel::Init, "UP");
        trace::pass!(Channel::Shell, "UP", "{}", touch());
        trace::event!(Channel::Shell, "E", n = touch());
    });
    if trace::ENABLED {
        check!(text == "INIT:UP:PASS\n", "got {text:?}");
    }
    check!(
        !EVALUATED.load(Ordering::Relaxed),
        "an argument of a masked trace was evaluated"
    );
    Ok(())
}

fn test_lines_ignore_the_mask() -> Result<(), String> {
    let text = captured("none", || {
        trace::test!("x:PASS");
        trace::test!("y:FAIL:{}", "bad");
    });
    check!(text == "TEST:x:PASS\nTEST:y:FAIL:bad\n", "got {text:?}");
    Ok(())
}

fn the_budget_prints_one_suppressed_line_per_window() -> Result<(), String> {
    if !trace::ENABLED {
        return Ok(());
    }
    let rate = Channel::Label.rate().ok_or("label has no rate")?;
    // A time far past anything an earlier test charged, so the window opens
    // fresh whatever the sink's budget already counted.
    let begin = 1 << 40;
    let text = captured("all", || {
        for _ in 0..rate.lines + 30 {
            trace::emit_at(begin, Channel::Label, |b| {
                let _ = core::fmt::Write::write_str(b, "LABEL:DENY x");
            });
        }
        trace::emit_at(begin + rate.window, Channel::Label, |b| {
            let _ = core::fmt::Write::write_str(b, "LABEL:DENY y");
        });
    });
    let notices = text
        .lines()
        .filter(|l| l.starts_with("LABEL:SUPPRESSED"))
        .count();
    check!(notices == 1, "{notices} notices in {text:?}");
    check!(
        text.contains("LABEL:SUPPRESSED n=30\n"),
        "wrong count in {text:?}"
    );
    check!(
        text.lines().filter(|l| *l == "LABEL:DENY x").count() == rate.lines as usize,
        "the window let a different number of lines through"
    );
    Ok(())
}

fn sysinfo_shows_the_mask() -> Result<(), String> {
    use crate::sysinfo;
    trace::set_mask_for_test(Mask::parse("init,ui", |_| {}));
    let mut words = alloc::vec![0u64; sysinfo::WORDS];
    let array: &mut [u64; sysinfo::WORDS] = words.as_mut_slice().try_into().map_err(|_| "size")?;
    sysinfo::snapshot_words(array);
    let seen = [
        words[sysinfo::H_TRACE_MASK_LO],
        words[sysinfo::H_TRACE_MASK_HI],
    ];
    trace::reset_for_test();
    let want = if trace::ENABLED {
        Mask::parse("init,ui", |_| {}).words()
    } else {
        [0, 0]
    };
    check!(seen == want, "sysinfo mask {seen:x?}, want {want:x?}");
    Ok(())
}

/// A traced line is in the program-output ring and not in the boot log.
#[cfg(lazyos_dbgd)]
fn traces_reach_the_program_ring_not_the_boot_log() -> Result<(), String> {
    use crate::klog;
    if !trace::ENABLED {
        return Ok(());
    }
    trace::pass!(Channel::Dev, "RINGCHECK", "{}", 0xC0FFEE);
    let mut programs = alloc::vec![0u8; klog::CAPACITY];
    let (count, _) = klog::programs_with_total(&mut programs);
    let programs = String::from_utf8_lossy(&programs[..count]).into_owned();
    let mut boot = alloc::vec![0u8; klog::CAPACITY];
    let count = klog::snapshot(&mut boot);
    let boot = String::from_utf8_lossy(&boot[..count]).into_owned();
    check!(
        programs.contains("DEV:RINGCHECK:PASS:12648430"),
        "missing from the program ring"
    );
    check!(
        !boot.contains("DEV:RINGCHECK"),
        "a trace landed in the boot log"
    );
    Ok(())
}

// ---- soaks -----------------------------------------------------------------

static MASKED_ARGS: AtomicU64 = AtomicU64::new(0);

/// A million lines on a masked channel complete in bounded time, evaluate
/// nothing, and do not touch the serial lock (the port is held locked
/// throughout: a masked line that took it would hang here).
fn masked_lines_are_cheap() -> Result<(), String> {
    fn arg() -> u64 {
        MASKED_ARGS.fetch_add(1, Ordering::Relaxed)
    }
    trace::set_mask_for_test(Mask::parse("init", |_| {}));
    let start = crate::boot_trace::tsc();
    crate::serial::with_port_locked(|| {
        for round in 0..1_000_000u64 {
            trace::pass!(Channel::Shell, "TICK", "{} {}", round, arg());
        }
    });
    let cycles = crate::boot_trace::tsc() - start;
    trace::reset_for_test();
    check!(
        MASKED_ARGS.load(Ordering::Relaxed) == 0,
        "arguments evaluated"
    );
    // 2 000 cycles a line is generous (an atomic load and a branch); the
    // bound exists to catch a masked line that formats or locks.
    check!(
        cycles < 2_000_000_000,
        "a million masked lines took {cycles} cycles"
    );
    Ok(())
}

const THREADS: usize = 2;
const PER_THREAD: u64 = 100_000;
const LIMIT_NS: u64 = 120_000_000_000;
static DONE: AtomicUsize = AtomicUsize::new(0);
static WHOLE: AtomicU64 = AtomicU64::new(0);
static TORN: AtomicU64 = AtomicU64::new(0);
static PARKED: crate::task::wait::WaitQueue =
    crate::task::wait::WaitQueue::new(crate::task::WaitKind::Sleep);

/// The shape every racing line has: `SHELL:TICK:PASS:t=<n> i=<n> pad=<x..>`
/// with `pad` as long as the line's `i` says, so a line mixed with another
/// task's fails somewhere.
fn judge(line: &[u8]) {
    let ok = core::str::from_utf8(line).is_ok_and(|text| {
        let Some(body) = text.strip_suffix('\n') else {
            return false;
        };
        let Some(rest) = body.strip_prefix("SHELL:TICK:PASS:t=") else {
            return false;
        };
        let mut parts = rest.split(' ');
        let (Some(_), Some(i), Some(pad), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        let want = i
            .strip_prefix("i=")
            .and_then(|n| n.parse::<usize>().ok())
            .map(|n| n % 64);
        pad.strip_prefix("pad=").map(str::len) == want
    });
    if ok {
        WHOLE.fetch_add(1, Ordering::Relaxed);
    } else {
        TORN.fetch_add(1, Ordering::Relaxed);
    }
}

extern "C" fn racer() -> ! {
    let me = crate::task::current();
    let id = DONE.load(Ordering::Relaxed) as u64;
    for i in 0..PER_THREAD {
        let pad = "x".repeat((i % 64) as usize);
        trace::pass!(Channel::Shell, "TICK", "t={id} i={i} pad={pad}");
    }
    DONE.fetch_add(1, Ordering::Relaxed);
    loop {
        x86_64::instructions::interrupts::disable();
        PARKED.wait_ns(me, None);
    }
}

fn kernel_only() {
    crate::task::register_kernel();
    crate::task::harness::reset();
    crate::task::harness::switch_current(crate::task::KERNEL_TASK);
    crate::task::set_blocked(false);
}

/// Two kernel threads race a hundred thousand lines each through the sink,
/// preempted by the tick: every line arrives whole, and the serial lock is
/// free afterwards.
fn racing_threads_never_tear_a_line() -> Result<(), String> {
    use crate::arch::{clock, irqchip};
    if !trace::ENABLED {
        return Ok(());
    }
    kernel_only();
    DONE.store(0, Ordering::Relaxed);
    WHOLE.store(0, Ordering::Relaxed);
    TORN.store(0, Ordering::Relaxed);
    trace::set_mask_for_test(Mask::all());
    capture::set(judge);
    let mut slots = Vec::new();
    for _ in 0..THREADS {
        let slot =
            crate::task::kthread::spawn_kernel_thread("trace-race", racer, PriorityClass::Normal)
                .map_err(|e| format!("spawn: {e}"))?;
        slots.push(slot);
    }
    let saved: [bool; 16] = core::array::from_fn(|l| irqchip::is_masked(l as u8));
    for line in 0..16u8 {
        irqchip::set_masked(line, line != 0);
    }
    let start = clock::monotonic_ns();
    x86_64::instructions::interrupts::enable();
    let mut timed_out = false;
    while DONE.load(Ordering::Relaxed) < THREADS {
        if clock::monotonic_ns() - start > LIMIT_NS {
            timed_out = true;
            break;
        }
        crate::task::idle_ns(clock::monotonic_ns() + 200_000);
    }
    x86_64::instructions::interrupts::disable();
    for (line, masked) in saved.into_iter().enumerate() {
        irqchip::set_masked(line as u8, masked);
    }
    capture::clear();
    trace::reset_for_test();
    for &slot in &slots {
        crate::task::harness::finish(slot, 0);
    }
    PARKED.notify_all();
    while crate::task::reap_child().is_some() {}
    crate::task::harness::reset();
    check!(
        !timed_out,
        "{} of {THREADS} threads finished",
        DONE.load(Ordering::Relaxed)
    );
    let (whole, torn) = (WHOLE.load(Ordering::Relaxed), TORN.load(Ordering::Relaxed));
    check!(torn == 0, "{torn} torn lines of {}", whole + torn);
    check!(
        whole == THREADS as u64 * PER_THREAD,
        "{whole} whole lines, want {}",
        THREADS as u64 * PER_THREAD
    );
    check!(!crate::serial::locked(), "the serial lock leaked");
    crate::trace::test!("trace_race_soak:INFO:{whole} lines whole");
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    ("trace_line_shapes", line_shapes),
    ("trace_details_stay_on_one_line", details_stay_on_one_line),
    (
        "trace_mask_comes_from_the_config",
        mask_comes_from_the_config,
    ),
    (
        "trace_masked_channel_writes_nothing",
        a_masked_channel_writes_nothing,
    ),
    (
        "trace_test_lines_ignore_the_mask",
        test_lines_ignore_the_mask,
    ),
    (
        "trace_budget_one_suppressed_line_per_window",
        the_budget_prints_one_suppressed_line_per_window,
    ),
    ("trace_sysinfo_shows_the_mask", sysinfo_shows_the_mask),
    #[cfg(lazyos_dbgd)]
    (
        "trace_reaches_program_ring_not_boot_log",
        traces_reach_the_program_ring_not_the_boot_log,
    ),
    ("trace_soak_masked_lines_are_cheap", masked_lines_are_cheap),
    (
        "trace_soak_racing_threads_never_tear",
        racing_threads_never_tear_a_line,
    ),
];
