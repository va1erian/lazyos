//! The serial port's writers under load (issue #857): a console write must
//! return in bounded time whatever else is logging, a full ring drops and
//! counts instead of draining for room, a caller with a fatal signal pending
//! is not held, and the repeats of an unimplemented syscall are counted, not
//! printed.
//!
//! The cases run against a diverted port (`serial::divert_for_test`): the
//! bytes go to the absent COM4 at once, a drain call may be capped to model a
//! slow consumer, and nothing may be printed to the real port meanwhile.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use super::*;
use crate::arch::clock::monotonic_ns;
use crate::process::linux::enosys;
use crate::serial;
use crate::task::signal::{self, SigInfo};
use crate::task::wait::WaitQueue;
use crate::task::{PriorityClass, WaitKind};

/// Ring capacity as the writers see it.
const RING: usize = 16 * 1024;

/// A kernel line of `bytes` bytes.
fn line(bytes: usize) -> String {
    let mut text = String::from("SERIALTEST:");
    while text.len() + 1 < bytes {
        text.push('.');
    }
    text.push('\n');
    text
}

/// `count` program lines of `width` bytes each, numbered and ending in a
/// newline: `PROG:0007:....`.
pub(super) fn numbered_lines(count: usize, width: usize) -> String {
    let mut text = String::new();
    for index in 0..count {
        let mut one = format!("PROG:{index:04}:");
        while one.len() + 1 < width {
            one.push('.');
        }
        one.push('\n');
        text.push_str(&one);
    }
    text
}

/// Fill the ring with kernel lines until it holds at least `bytes`.
fn fill_to(bytes: usize) {
    let text = line(200);
    while serial::queued_len() < bytes {
        serial::_write_str(&text);
    }
}

/// A stalled consumer (nothing ever drains): a program write ends, bounded
/// by its rounds, and what does not fit is dropped and counted.
fn mirror_ends_on_a_stalled_port() -> Result<(), String> {
    let port = serial::divert_for_test(0);
    fill_to(RING - 3000);
    let rounds = serial::MIRROR_ROUNDS.load(Ordering::Relaxed);
    let start = monotonic_ns();
    serial::mirror(&[b'x'; 100]);
    let took = monotonic_ns() - start;
    let ran = serial::MIRROR_ROUNDS.load(Ordering::Relaxed) - rounds;
    // The program may not fill the last 2 KiB: those lines were dropped.
    serial::mirror(numbered_lines(80, 100).as_bytes());
    let held = serial::queued_len();
    let dropped = serial::dropped_total();
    drop(port);
    check!(
        took < 500_000_000,
        "a write to a stalled port took {took} ns"
    );
    check!(
        ran > 0 && ran <= 600,
        "{ran} drain rounds against a port that never empties"
    );
    check!(
        held <= RING - 2048 + 1,
        "a program filled the ring to {held} bytes, into the kernel's reserve"
    );
    check!(dropped > 0, "the bytes that did not fit were not counted");
    Ok(())
}

/// The kernel's reserve: after programs have filled their part of the ring,
/// a kernel line still queues; once the ring is truly full it is dropped
/// and counted, never drained for room.
fn kernel_lines_use_the_reserve() -> Result<(), String> {
    let port = serial::divert_for_test(0);
    serial::mirror(numbered_lines(3 * RING / 100, 100).as_bytes());
    let program_fill = serial::queued_len();
    serial::_write_str(&line(300));
    let with_line = serial::queued_len();
    for _ in 0..40 {
        serial::_write_str(&line(300));
    }
    let full = serial::queued_len();
    let (queued_before, sent_before) = serial::counters_for_test();
    let dropped = serial::dropped_total();
    drop(port);
    check!(
        program_fill < RING,
        "program output filled the whole ring ({program_fill})"
    );
    check!(
        with_line > program_fill,
        "a kernel line found no room in the reserve ({program_fill} -> {with_line})"
    );
    check!(full <= RING, "the ring holds {full} bytes of {RING}");
    check!(dropped > 0, "nothing was counted as dropped");
    check!(
        queued_before.wrapping_sub(sent_before) == full,
        "queued {queued_before} sent {sent_before}: not {full} waiting"
    );
    Ok(())
}

/// A caller with a fatal signal pending is not held: `mirror` runs no drain
/// round for it, while the same call unkilled does.
fn mirror_returns_to_a_killed_caller() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    signal::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    let child = task::spawn_fork().map_err(|error| format!("spawn: {error}"))?;
    let port = serial::divert_for_test(0);
    task::harness::switch_current(child);
    let before = serial::MIRROR_ROUNDS.load(Ordering::Relaxed);
    serial::mirror(b"alive\n");
    let alive = serial::MIRROR_ROUNDS.load(Ordering::Relaxed) - before;
    signal::send_to_slot(task::KERNEL_TASK, child, signal::SIGKILL, SigInfo::kernel())
        .map_err(|error| format!("SIGKILL: {error:?}"))?;
    let killed = signal::killed(child);
    let before = serial::MIRROR_ROUNDS.load(Ordering::Relaxed);
    serial::mirror(b"dying\n");
    let rounds = serial::MIRROR_ROUNDS.load(Ordering::Relaxed) - before;
    // ...nor while it waits for room in a full ring.
    fill_to(RING - 100);
    let before = serial::MIRROR_ROUNDS.load(Ordering::Relaxed);
    serial::mirror(numbered_lines(40, 100).as_bytes());
    let waiting = serial::MIRROR_ROUNDS.load(Ordering::Relaxed) - before;
    drop(port);
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::finish(child, 0);
    while task::reap_child().is_some() {}
    signal::harness::reset();
    task::harness::reset();
    check!(alive > 0, "an unkilled write ran no rounds");
    check!(killed, "SIGKILL was not pending");
    check!(rounds == 0, "a killed caller drained for {rounds} rounds");
    check!(
        waiting == 0,
        "a killed caller waited {waiting} rounds for room"
    );
    Ok(())
}

/// The caller's own last byte is the end of its wait: with a consumer that
/// takes a few bytes a call, a write behind a backlog returns once the
/// backlog and its bytes are out, and not before.
fn mirror_waits_for_its_own_bytes() -> Result<(), String> {
    let port = serial::divert_for_test(64);
    let text = line(400);
    for _ in 0..5 {
        serial::_write_str(&text);
    }
    let (queued, _) = serial::counters_for_test();
    serial::mirror(b"mine\n");
    let (queued_after, sent_after) = serial::counters_for_test();
    drop(port);
    check!(
        queued_after.wrapping_sub(queued) == 5,
        "the write queued {} bytes",
        queued_after.wrapping_sub(queued)
    );
    // The consumer takes 64 bytes a call and the backlog is 2000: the wait
    // ends when the write's own last byte is out, and that is everything
    // queued ahead of it.
    check!(
        sent_after == queued_after,
        "sent {sent_after} of {queued_after}: the write returned before its last byte went out"
    );
    Ok(())
}

/// The kernel task's drain reports what was dropped, once, when there is room.
fn drops_are_reported_once() -> Result<(), String> {
    static LINES: spin::Mutex<Vec<u8>> = spin::Mutex::new(Vec::new());
    fn store(line: &[u8]) {
        x86_64::instructions::interrupts::without_interrupts(|| {
            LINES.lock().extend_from_slice(line)
        });
    }
    if !crate::trace::ENABLED {
        return Ok(());
    }
    let port = serial::divert_for_test(0);
    fill_to(RING - 100);
    for _ in 0..30 {
        serial::_write_str(&line(300));
    }
    // Still full: nothing is said.
    crate::trace::capture::set(store);
    serial::report_dropped();
    serial::set_cap_for_test(usize::MAX);
    serial::service();
    serial::report_dropped();
    crate::trace::capture::clear();
    drop(port);
    let text = x86_64::instructions::interrupts::without_interrupts(|| {
        String::from_utf8_lossy(&LINES.lock()).into_owned()
    });
    LINES.lock().clear();
    let reports = text.matches("SERIAL:DROPPED").count();
    check!(reports == 1, "{reports} drop reports in {text:?}");
    check!(text.contains("bytes="), "the report has no count: {text:?}");
    Ok(())
}

// ---- writers against a flood ---------------------------------------------

/// Set to end the threads.
static STOP: AtomicBool = AtomicBool::new(false);
/// Writers' progress: completed calls, the slowest finished call (ns), and
/// when the call in progress started (0: none).
static CALLS: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static SLOWEST: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
static IN_CALL: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
/// Lines the flooder wrote.
static FLOODED: AtomicU64 = AtomicU64::new(0);
static NEXT_WRITER: AtomicUsize = AtomicUsize::new(0);
static PARK: WaitQueue = WaitQueue::new(WaitKind::Sleep);

fn park_forever() -> ! {
    let me = task::current();
    loop {
        x86_64::instructions::interrupts::disable();
        PARK.wait_ns(me, None);
    }
}

/// A program writing to its console in a loop, like a syscall (interrupts
/// off between the breaths).
extern "C" fn writer() -> ! {
    let index = NEXT_WRITER.fetch_add(1, Ordering::Relaxed) % 2;
    let text = line(80);
    while !STOP.load(Ordering::Relaxed) {
        let start = monotonic_ns();
        IN_CALL[index].store(start, Ordering::Relaxed);
        serial::mirror(text.as_bytes());
        IN_CALL[index].store(0, Ordering::Relaxed);
        SLOWEST[index].fetch_max(monotonic_ns() - start, Ordering::Relaxed);
        CALLS[index].fetch_add(1, Ordering::Relaxed);
        crate::block::iowait::breathe(crate::block::Wait::MaySleep);
    }
    park_forever()
}

/// The kernel logging as fast as it can.
extern "C" fn flooder() -> ! {
    let text = line(120);
    while !STOP.load(Ordering::Relaxed) {
        serial::_write_str(&text);
        FLOODED.fetch_add(1, Ordering::Relaxed);
        crate::block::iowait::breathe(crate::block::Wait::MaySleep);
    }
    park_forever()
}

/// Two console writers and a flooder against a consumer that takes `cap`
/// bytes a drain call, for `run_ns`: every write returns within `bound_ns`
/// (the one in progress at the end included), both writers progress, and the
/// ring never holds more than its size. Returns the lines flooded.
fn flood_round(cap: usize, run_ns: u64, bound_ns: u64) -> Result<u64, String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    task::set_blocked(false);
    STOP.store(false, Ordering::Relaxed);
    NEXT_WRITER.store(0, Ordering::Relaxed);
    FLOODED.store(0, Ordering::Relaxed);
    for index in 0..2 {
        CALLS[index].store(0, Ordering::Relaxed);
        SLOWEST[index].store(0, Ordering::Relaxed);
        IN_CALL[index].store(0, Ordering::Relaxed);
    }
    let mut slots = Vec::new();
    for (name, entry) in [
        ("serial-a", writer as extern "C" fn() -> !),
        ("serial-b", writer),
        ("serial-flood", flooder),
    ] {
        let slot = task::kthread::spawn_kernel_thread(name, entry, PriorityClass::Normal)
            .map_err(|error| format!("spawn {name}: {error}"))?;
        slots.push(slot);
    }
    let port = serial::divert_for_test(cap);
    crate::block::iowait::set_test_sleep(true);
    x86_64::instructions::interrupts::enable();
    let begin = monotonic_ns();
    task::idle_ns(begin + run_ns);
    let end = monotonic_ns();
    x86_64::instructions::interrupts::disable();
    let stuck: Vec<u64> = (0..2)
        .map(|i| match IN_CALL[i].load(Ordering::Relaxed) {
            0 => 0,
            since => end.saturating_sub(since),
        })
        .collect();
    let calls = [
        CALLS[0].load(Ordering::Relaxed),
        CALLS[1].load(Ordering::Relaxed),
    ];
    let slowest = [
        SLOWEST[0].load(Ordering::Relaxed).max(stuck[0]),
        SLOWEST[1].load(Ordering::Relaxed).max(stuck[1]),
    ];
    let flooded = FLOODED.load(Ordering::Relaxed);
    let held = serial::queued_len();
    let dropped = serial::dropped_total();
    STOP.store(true, Ordering::Relaxed);
    crate::block::iowait::set_test_sleep(false);
    drop(port);
    task::harness::switch_current(task::KERNEL_TASK);
    for &slot in &slots {
        task::harness::finish(slot, 0);
    }
    PARK.notify_all();
    while task::reap_child().is_some() {}
    task::harness::reset();
    // A consumer that takes (almost) nothing leaves every write waiting out
    // its bound, so few of them fit in the run.
    let wanted = if cap < 64 { 2 } else { 5 };
    for index in 0..2 {
        check!(
            calls[index] >= wanted,
            "cap {cap}: writer {index} completed only {} writes (flooded {flooded})",
            calls[index]
        );
        check!(
            slowest[index] < bound_ns,
            "cap {cap}: writer {index}'s slowest write took {} ms",
            slowest[index] / 1_000_000
        );
    }
    check!(held <= RING, "cap {cap}: the ring holds {held} bytes");
    check!(flooded > 0, "cap {cap}: the flooder never ran");
    if cap < 256 {
        check!(
            dropped > 0,
            "cap {cap}: a flooded slow port dropped nothing"
        );
    }
    Ok(flooded)
}

/// Two writers and a flooder, a slow consumer: writes stay bounded and the
/// writers keep going (before the fix a writer waited for an empty ring and
/// never left the kernel).
fn writers_survive_a_flood() -> Result<(), String> {
    flood_round(64, 400_000_000, 250_000_000).map(|_| ())
}

/// Soak: the same against consumers from hopeless to fast, several rounds
/// each, and a fast consumer loses (almost) nothing.
fn soak_floods_at_every_speed() -> Result<(), String> {
    for round in 0..3 {
        for cap in [0usize, 16, 64, 1024, usize::MAX] {
            flood_round(cap, 250_000_000, 250_000_000)
                .map_err(|error| format!("round {round}: {error}"))?;
        }
    }
    Ok(())
}

// ---- unimplemented syscalls ----------------------------------------------

/// An `ENOSYS` repeated a hundred times prints once and is counted; the
/// count line comes once per period.
fn enosys_repeats_are_counted() -> Result<(), String> {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    enosys::reset_for_test();
    // 500 is no Linux syscall this kernel implements.
    for _ in 0..100 {
        let result = crate::process::linux::dispatch_for_test(500, 0, 0, 0);
        check!(
            result == (-38i64) as u64,
            "syscall 500 answered {result:#x}, not -ENOSYS"
        );
    }
    let counts = enosys::counts_for_test(500, u64::MAX);
    check!(
        counts == Some((100, 1)),
        "(count, printed) after 100 calls: {counts:?}"
    );
    enosys::report_at_for_test(5_000);
    let counts = enosys::counts_for_test(500, u64::MAX);
    check!(
        counts == Some((100, 100)),
        "after a report: {counts:?}, wanted all printed"
    );
    // A second report in the same period says nothing new.
    crate::process::linux::dispatch_for_test(500, 0, 0, 0);
    enosys::report_at_for_test(5_100);
    check!(
        enosys::counts_for_test(500, u64::MAX) == Some((101, 100)),
        "a report inside the period printed the repeat"
    );
    enosys::report_at_for_test(6_100);
    check!(
        enosys::counts_for_test(500, u64::MAX) == Some((101, 101)),
        "the next period did not print the repeat"
    );
    enosys::reset_for_test();
    Ok(())
}

/// Soak: far more distinct calls than the table holds, each repeated: the
/// table replaces its oldest entries, counts stay consistent (never more
/// printed than counted), and nothing panics.
fn soak_enosys_table_turns_over() -> Result<(), String> {
    // ~8000 first-time lines: to a port that swallows them, not the real one.
    let _port = serial::divert_for_test(usize::MAX);
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    enosys::reset_for_test();
    for round in 0..40u64 {
        for sub in 0..200u64 {
            enosys::note_for_test(202, sub);
            enosys::note_for_test(202, sub);
        }
        enosys::report_at_for_test(10_000 * (round + 1));
        if let Some((count, printed)) = enosys::counts_for_test(202, 199) {
            check!(printed <= count, "printed {printed} of {count}");
        }
    }
    enosys::reset_for_test();
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "serial_mirror_ends_on_a_stalled_port",
        mirror_ends_on_a_stalled_port,
    ),
    (
        "serial_kernel_lines_use_the_reserve",
        kernel_lines_use_the_reserve,
    ),
    (
        "serial_mirror_returns_to_a_killed_caller",
        mirror_returns_to_a_killed_caller,
    ),
    (
        "serial_mirror_waits_for_its_own_bytes",
        mirror_waits_for_its_own_bytes,
    ),
    ("serial_drops_are_reported_once", drops_are_reported_once),
    ("serial_writers_survive_a_flood", writers_survive_a_flood),
    (
        "serial_soak_floods_at_every_speed",
        soak_floods_at_every_speed,
    ),
    (
        "serial_enosys_repeats_are_counted",
        enosys_repeats_are_counted,
    ),
    (
        "serial_soak_enosys_table_turns_over",
        soak_enosys_table_turns_over,
    ),
];
