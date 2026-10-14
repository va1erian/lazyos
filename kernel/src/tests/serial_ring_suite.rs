//! The serial ring's guarantees (issue #857): evidence lines are whole or
//! absent, a program write waits for room before it drops, the kernel task's
//! drain never prints, and a UART that stopped emptying costs a writer about a
//! millisecond and then leaves the port alone for a growing while.
//!
//! Like `serial_suite`, against a diverted port: nothing may be printed to the
//! real one meanwhile.

use super::serial_suite::numbered_lines;
use super::*;
use crate::serial;

const RING: usize = 16 * 1024;

/// The kernel's own test line, `bytes` long, ending in a newline.
fn kernel_line(bytes: usize) -> String {
    let mut text = String::from("SERIALTEST:");
    while text.len() + 1 < bytes {
        text.push('.');
    }
    text.push('\n');
    text
}

/// The queued bytes as lines, and whether the last one ends in a newline.
fn ring_lines() -> (Vec<String>, bool) {
    let bytes = serial::ring_contents_for_test();
    let ends = bytes.last().is_none_or(|&b| b == b'\n');
    let text = String::from_utf8_lossy(&bytes).into_owned();
    (text.lines().map(String::from).collect(), ends)
}

/// Every line in the ring starts with exactly one known marker (`PROG:` or
/// `SERIALTEST:` or `TRACETEST:`); those that are `whole` have the expected
/// width. Kernel text may be cut short (and still ends), never merged.
fn check_lines(whole_prefixes: &[(&str, usize)]) -> Result<(), String> {
    let (lines, ends) = ring_lines();
    // A line of kernel text cut by a ring that is truly full has lost its
    // newline too; it is ended when the next line starts.
    check!(
        ends || serial::queued_len() == RING,
        "the ring ends in the middle of a line"
    );
    for (index, line) in lines.iter().enumerate() {
        if line.is_empty() {
            continue; // the newline that ends a cut line
        }
        // A consumer that took part of the oldest line leaves its tail first.
        let known = ["PROG:", "SERIALTEST:", "TRACETEST:"];
        if index == 0 && !known.iter().any(|marker| line.starts_with(marker)) {
            continue;
        }
        let markers = ["PROG:", "SERIALTEST:", "TRACETEST:"]
            .iter()
            .map(|marker| line.matches(marker).count())
            .sum::<usize>();
        check!(
            markers == 1 || (markers == 0 && line.len() < 11),
            "a line carries {markers} markers: {line:?}"
        );
        for (prefix, width) in whole_prefixes {
            if line.starts_with(prefix) {
                check!(
                    line.len() == width - 1,
                    "a {prefix} line was cut to {} bytes of {}: {line:?}",
                    line.len(),
                    width - 1
                );
            }
        }
    }
    Ok(())
}

/// A full ring holds program lines and trace lines whole; kernel text that
/// did not fit is cut at most, and the cut ends in a newline.
fn lines_are_whole_or_absent() -> Result<(), String> {
    let port = serial::divert_for_test(0);
    serial::mirror(numbered_lines(300, 100).as_bytes());
    check_lines(&[("PROG:", 100)])?;
    // Traces: whole or not at all.
    let trace = format!("TRACETEST:{}\n", ".".repeat(109));
    for _ in 0..200 {
        serial::write_trace(trace.as_bytes());
    }
    check_lines(&[("PROG:", 100), ("TRACETEST:", 120)])?;
    // Kernel text, until the ring is truly full.
    for _ in 0..80 {
        serial::_write_str(&kernel_line(300));
    }
    check_lines(&[("PROG:", 100), ("TRACETEST:", 120)])?;
    let dropped = serial::dropped_total();
    // Drain, and write once more: the cut line (if any) is ended first.
    serial::set_cap_for_test(usize::MAX);
    serial::service();
    check!(serial::queued_len() == 0, "the ring did not drain");
    serial::set_cap_for_test(0); // keep the next line in the ring to look at
    serial::_write_str("SERIALTEST:after\n");
    let (lines, ends) = ring_lines();
    drop(port);
    check!(dropped > 0, "nothing was dropped from a full ring");
    check!(ends, "the new line is not ended");
    check!(
        lines.last().map(String::as_str) == Some("SERIALTEST:after")
            && lines.iter().filter(|l| !l.is_empty()).count() == 1,
        "the line after a cut ran into it: {lines:?}"
    );
    Ok(())
}

/// A program write that does not fit waits for the consumer to make room and
/// loses nothing; against a consumer that takes nothing it loses whole lines.
fn program_write_waits_for_room() -> Result<(), String> {
    let port = serial::divert_for_test(64);
    let kernel = kernel_line(200);
    while serial::queued_len() < RING - 2048 - 300 {
        serial::_write_str(&kernel);
    }
    let before = serial::dropped_total();
    serial::mirror(numbered_lines(40, 100).as_bytes());
    let lost = serial::dropped_total() - before;
    check!(lost == 0, "{lost} bytes dropped by a write that could wait");
    check_lines(&[("PROG:", 100)])?;
    drop(port);

    let port = serial::divert_for_test(0);
    while serial::queued_len() < RING - 2048 - 300 {
        serial::_write_str(&kernel);
    }
    let before = serial::dropped_total();
    serial::mirror(numbered_lines(40, 100).as_bytes());
    let lost = serial::dropped_total() - before;
    check!(lost > 0, "nothing dropped though nothing could drain");
    check!(lost % 100 == 0, "{lost} bytes dropped: not whole lines");
    check_lines(&[("PROG:", 100)])?;
    drop(port);
    Ok(())
}

/// A write whose lines were all dropped returns without the second wait for
/// its own bytes (it has none).
fn dropped_write_returns_at_once() -> Result<(), String> {
    let port = serial::divert_for_test(0);
    let kernel = kernel_line(200);
    while serial::queued_len() < RING - 1500 {
        serial::_write_str(&kernel);
    }
    let (queued, _) = serial::counters_for_test();
    let start = crate::perf::rdtsc();
    serial::mirror(numbered_lines(3, 100).as_bytes());
    let took = super::serial_suite::tsc_ms(crate::perf::rdtsc() - start);
    let (after, _) = serial::counters_for_test();
    drop(port);
    check!(
        after == queued,
        "{} bytes of a dropped write queued",
        after - queued
    );
    // One wait (50 ms for room); a second, for own bytes (20 ms), would be
    // more. Time, not rounds: a port in back-off makes rounds free.
    check!(
        (30..75).contains(&took),
        "{took} ms for a write that queued nothing (one 50 ms wait for room)"
    );
    Ok(())
}

/// The kernel task's drain prints nothing and takes the port only with
/// interrupts off, so a task preempted in it cannot hold the lock a syscall
/// that logs with interrupts off then spins on (the #382 rule).
fn service_keeps_the_port_with_interrupts_off() -> Result<(), String> {
    static LINES: spin::Mutex<Vec<u8>> = spin::Mutex::new(Vec::new());
    fn store(line: &[u8]) {
        x86_64::instructions::interrupts::without_interrupts(|| {
            LINES.lock().extend_from_slice(line)
        });
    }
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
    let port = serial::divert_for_test(usize::MAX);
    // Drops pending, so a drain that reported them would print.
    serial::set_cap_for_test(0);
    for _ in 0..100 {
        serial::_write_str(&kernel_line(300));
    }
    serial::set_cap_for_test(usize::MAX);
    x86_64::instructions::interrupts::without_interrupts(|| LINES.lock().clear());
    crate::trace::capture::set(store);
    serial::watch_interrupt_locks(true);
    // As the multiplexer calls it: interrupts on.
    x86_64::instructions::interrupts::enable();
    serial::service();
    x86_64::instructions::interrupts::disable();
    let violations = serial::watch_interrupt_locks(false);
    crate::trace::capture::clear();
    // The report is for interrupts off, and does say it.
    crate::trace::reset_for_test();
    crate::trace::capture::set(store);
    serial::report();
    crate::trace::capture::clear();
    drop(port);
    let printed = x86_64::instructions::interrupts::without_interrupts(|| {
        String::from_utf8_lossy(&LINES.lock()).into_owned()
    });
    check!(
        violations == 0,
        "the port was locked {violations} times with interrupts on"
    );
    let reports = printed.matches("SERIAL:DROPPED").count();
    check!(
        reports == 1 || !crate::trace::ENABLED,
        "service() printed, or report() did not: {printed:?}"
    );
    Ok(())
}

/// A UART that never reports empty costs one drain about a millisecond, the
/// next ones nothing for a while, and the wait before the next try doubles.
fn stalled_uart_is_bounded_in_time() -> Result<(), String> {
    use crate::perf::rdtsc;
    let port = serial::divert_for_test(usize::MAX);
    serial::set_never_empty_for_test(true);
    let per_tick = crate::arch::clock::cycles_per_tick().max(1);
    let ms = |cycles: u64| cycles * 10 / per_tick;
    let line = kernel_line(100);

    let began = rdtsc();
    serial::_write_str(&line);
    let first = ms(rdtsc() - began);
    check!(
        first < 25,
        "the first write to a stalled UART took {first} ms"
    );
    check!(serial::stall_level_for_test() == 1, "no give-up recorded");

    let began = rdtsc();
    for _ in 0..20 {
        serial::_write_str(&line);
    }
    let backed_off = ms(rdtsc() - began);
    check!(
        backed_off < 25,
        "20 writes during the back-off took {backed_off} ms"
    );
    check!(
        serial::stall_level_for_test() == 1,
        "a drain tried the port during its back-off"
    );

    // Wait the first back-off out (2 ticks), try again: the second give-up.
    let began = rdtsc();
    while rdtsc() - began < per_tick * 3 {
        core::hint::spin_loop();
    }
    serial::_write_str(&line);
    check!(
        serial::stall_level_for_test() == 2,
        "level {} after the first back-off ended",
        serial::stall_level_for_test()
    );
    // Not 2 ticks again but 4: 3 ticks later it is still backed off.
    let began = rdtsc();
    while rdtsc() - began < per_tick * 3 {
        core::hint::spin_loop();
    }
    serial::_write_str(&line);
    check!(
        serial::stall_level_for_test() == 2,
        "the second back-off was not longer than the first"
    );

    // The consumer wakes up: everything goes out, and the level resets.
    serial::set_never_empty_for_test(false);
    serial::set_cap_for_test(usize::MAX);
    serial::service();
    check!(
        serial::queued_len() == 0 && serial::stall_level_for_test() == 0,
        "the port did not recover: {} bytes, level {}",
        serial::queued_len(),
        serial::stall_level_for_test()
    );
    drop(port);
    Ok(())
}

/// Soak: program, trace and kernel writers against a consumer that comes and
/// goes. At every look the ring holds whole program and trace lines, kernel
/// text at worst cut and ended, and drops are all counted.
fn soak_mixed_writers_keep_lines_whole() -> Result<(), String> {
    let port = serial::divert_for_test(0);
    let trace = format!("TRACETEST:{}\n", ".".repeat(79));
    let kernel = kernel_line(150);
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for round in 0..800 {
        match next() % 8 {
            0..=2 => serial::mirror(numbered_lines(1 + (next() % 4) as usize, 100).as_bytes()),
            3..=4 => serial::write_trace(trace.as_bytes()),
            5..=6 => serial::_write_str(&kernel),
            _ => {
                let cap = [0usize, 64, 1024, usize::MAX][(next() % 4) as usize];
                serial::set_cap_for_test(cap);
                serial::service();
            }
        }
        if round % 100 == 99 {
            check_lines(&[("PROG:", 100), ("TRACETEST:", 90)])
                .map_err(|error| format!("round {round}: {error}"))?;
            check!(
                serial::queued_len() <= RING,
                "round {round}: the ring holds {}",
                serial::queued_len()
            );
        }
    }
    drop(port);
    Ok(())
}

/// A multi-megabyte write costs bounded time and memory: at most a ring's
/// worth is set aside, the rest is dropped (counted), and what is queued is
/// whole lines.
fn huge_write_is_bounded() -> Result<(), String> {
    let text = numbered_lines(25_000, 100); // 2.5 MB
    let port = serial::divert_for_test(0);
    let heap_before = crate::mem::heap_stats().used;
    let start = crate::perf::rdtsc();
    serial::mirror(text.as_bytes());
    let took = super::serial_suite::tsc_ms(crate::perf::rdtsc() - start);
    let heap_after = crate::mem::heap_stats().used;
    let dropped = serial::dropped_total();
    check_lines(&[("PROG:", 100)])?;
    let held = serial::queued_len();
    drop(port);
    drop(text);
    check!(took < 200, "a 2.5 MB write took {took} ms");
    check!(
        heap_after <= heap_before + 64 * 1024,
        "the write held {} bytes of heap",
        heap_after - heap_before
    );
    check!(
        dropped as usize + held >= 2_400_000,
        "{dropped} dropped and {held} queued of 2.5 MB: bytes unaccounted for"
    );
    Ok(())
}

/// A port that never takes a byte (in back-off, drains return at once) still
/// bounds a write by real time: the wait is the 20 ms it says, not the few
/// microseconds a count of rounds allowed.
fn wait_is_bounded_in_time_not_rounds() -> Result<(), String> {
    let port = serial::divert_for_test(usize::MAX);
    serial::set_never_empty_for_test(true);
    // The first write finds the port stalled and puts it in back-off.
    serial::_write_str(&kernel_line(100));
    let before = serial::MIRROR_ROUNDS.load(core::sync::atomic::Ordering::Relaxed);
    let start = crate::perf::rdtsc();
    serial::mirror(numbered_lines(1, 100).as_bytes());
    let took = super::serial_suite::tsc_ms(crate::perf::rdtsc() - start);
    let rounds = serial::MIRROR_ROUNDS.load(core::sync::atomic::Ordering::Relaxed) - before;
    serial::set_never_empty_for_test(false);
    drop(port);
    check!(
        (15..60).contains(&took),
        "a write behind a stalled port waited {took} ms, not about 20"
    );
    check!(
        rounds > 600,
        "only {rounds} rounds in {took} ms: not waiting on the clock"
    );
    Ok(())
}

/// The last line of a panic does not wait for a port lock its context holds,
/// and says if bytes were dropped before it.
fn final_line_survives_a_held_lock() -> Result<(), String> {
    let port = serial::divert_for_test(0);
    for _ in 0..80 {
        serial::_write_str(&kernel_line(300));
    }
    check!(
        serial::dropped_total() > 0,
        "nothing dropped: the case proves nothing"
    );
    serial::set_cap_for_test(usize::MAX);
    let (queued, _) = serial::counters_for_test();
    serial::leak_port_lock_for_test();
    serial::final_line(format_args!("SERIALTEST:last words"));
    let locked = serial::port_locked_for_test();
    let (after, sent) = serial::counters_for_test();
    drop(port);
    check!(!locked, "the lock is still held");
    check!(
        after == sent,
        "the final line is not out: {after} queued, {sent} sent"
    );
    check!(
        after - queued >= "SERIALTEST:last words\nSERIAL:DROPPED bytes=".len(),
        "no word of the drops after the last line ({} bytes)",
        after - queued
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "serial_ring_lines_are_whole_or_absent",
        lines_are_whole_or_absent,
    ),
    (
        "serial_ring_program_write_waits_for_room",
        program_write_waits_for_room,
    ),
    (
        "serial_ring_dropped_write_returns_at_once",
        dropped_write_returns_at_once,
    ),
    (
        "serial_ring_service_keeps_the_port_with_interrupts_off",
        service_keeps_the_port_with_interrupts_off,
    ),
    (
        "serial_ring_stalled_uart_is_bounded_in_time",
        stalled_uart_is_bounded_in_time,
    ),
    ("serial_ring_huge_write_is_bounded", huge_write_is_bounded),
    (
        "serial_ring_wait_is_bounded_in_time_not_rounds",
        wait_is_bounded_in_time_not_rounds,
    ),
    (
        "serial_ring_final_line_survives_a_held_lock",
        final_line_survives_a_held_lock,
    ),
    (
        "serial_ring_soak_mixed_writers_keep_lines_whole",
        soak_mixed_writers_keep_lines_whole,
    ),
];
