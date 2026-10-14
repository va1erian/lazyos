//! Reports of unimplemented Linux syscalls, rate limited (issue #857).
//!
//! A program that meets an unimplemented call usually meets it again and
//! again (a `fallocate` per cache body: 45 lines in one page load), and each
//! `ENOSYS` line was printed from inside the syscall, with interrupts off, into
//! the serial ring the next one was waiting for. Now the first `ENOSYS` of a
//! (process, call) prints at once, exactly as before (`tools/abi/coverage.py`
//! reads that line), and the repeats are only counted; the kernel task's loop
//! prints one count line per call every [`REPORT_TICKS`] (`report`).

use spin::Mutex;

use crate::task;

/// Ticks (100 Hz) between two count lines.
const REPORT_TICKS: u64 = 1000;
/// (process, call) pairs remembered; past it the oldest entry is replaced.
const ENTRIES: usize = 64;

/// What is known of one (process, call) pair.
#[derive(Clone, Copy)]
struct Entry {
    /// The address space (a recycled one is told apart by `comm`).
    pml4: u64,
    comm: [u8; 16],
    nr: u64,
    /// `futex` operation and the like; `u64::MAX` for none.
    sub: u64,
    /// Calls so far, and the ones already printed.
    count: u64,
    printed: u64,
}

struct Table {
    entries: [Option<Entry>; ENTRIES],
    /// Next entry to replace when the table is full.
    victim: usize,
    last_report: u64,
}

static TABLE: Mutex<Table> = Mutex::new(Table {
    entries: [None; ENTRIES],
    victim: 0,
    last_report: 0,
});

/// Note that the calling process met unimplemented call `nr` (`sub` is an
/// operation within it, or `u64::MAX`). Prints the first time only. Syscall
/// context (interrupts off).
pub(super) fn note(nr: u64, sub: u64) {
    let me = task::current();
    let (comm, _) = task::linuxstate::comm(me);
    let Some(pml4) = task::pml4_of(me) else {
        return;
    };
    let first = {
        let mut table = TABLE.lock();
        match table
            .entries
            .iter_mut()
            .flatten()
            .find(|e| e.pml4 == pml4 && e.nr == nr && e.sub == sub && e.comm == comm)
        {
            Some(entry) => {
                entry.count += 1;
                false
            }
            None => {
                let slot = table
                    .entries
                    .iter()
                    .position(Option::is_none)
                    .unwrap_or_else(|| {
                        let victim = table.victim;
                        table.victim = (victim + 1) % ENTRIES;
                        victim
                    });
                table.entries[slot] = Some(Entry {
                    pml4,
                    comm,
                    nr,
                    sub,
                    count: 1,
                    printed: 1,
                });
                true
            }
        }
    };
    if first {
        print_first(nr, sub);
    }
}

/// The first line: `ENOSYS <nr> <name>`, with the operation if there is one.
fn print_first(nr: u64, sub: u64) {
    if sub == u64::MAX {
        crate::serial_println!("ENOSYS {} {}", nr, super::names::syscall_name(nr));
    } else {
        crate::serial_println!(
            "ENOSYS {} {} op {}",
            nr,
            super::names::syscall_name(nr),
            sub
        );
    }
}

/// Print one line per call that repeated since the last report, every
/// [`REPORT_TICKS`]: `ENOSYS <nr> <name> count=<n> comm=<name>`. The kernel
/// task's loop calls it (interrupts off, as a kernel section).
pub fn report() {
    report_at(task::ticks());
}

/// [`report`] at tick `now`.
fn report_at(now: u64) {
    let mut lines: [Option<Entry>; ENTRIES] = [None; ENTRIES];
    {
        let mut table = TABLE.lock();
        if now.wrapping_sub(table.last_report) < REPORT_TICKS {
            return;
        }
        table.last_report = now;
        for (line, entry) in lines.iter_mut().zip(table.entries.iter_mut()) {
            if let Some(e) = entry.as_mut().filter(|e| e.count > e.printed) {
                *line = Some(*e);
                e.printed = e.count;
            }
        }
    }
    for entry in lines.iter().flatten() {
        let len = entry.comm.iter().position(|&b| b == 0).unwrap_or(16);
        let comm = core::str::from_utf8(&entry.comm[..len]).unwrap_or("?");
        let repeats = entry.count - entry.printed.min(entry.count);
        let name = super::names::syscall_name(entry.nr);
        // The copy was taken before `printed` caught up: the difference is
        // what is new.
        crate::serial_println!(
            "ENOSYS {} {} count={} comm={}",
            entry.nr,
            name,
            repeats,
            comm
        );
    }
}

/// Forget everything (the kernel suite).
#[cfg(lazyos_tests)]
pub fn reset_for_test() {
    let mut table = TABLE.lock();
    table.entries = [None; ENTRIES];
    table.victim = 0;
    table.last_report = 0;
}

/// Calls counted and printed for a (process, call) pair of the current task
/// (the kernel suite): `(count, printed)`.
#[cfg(lazyos_tests)]
pub fn counts_for_test(nr: u64, sub: u64) -> Option<(u64, u64)> {
    let me = task::current();
    let pml4 = task::pml4_of(me)?;
    let table = TABLE.lock();
    table
        .entries
        .iter()
        .flatten()
        .find(|e| e.pml4 == pml4 && e.nr == nr && e.sub == sub)
        .map(|e| (e.count, e.printed))
}

/// [`report`] at a chosen tick (the kernel suite).
#[cfg(lazyos_tests)]
pub fn report_at_for_test(now: u64) {
    report_at(now);
}

/// [`note`] for the calling task (the kernel suite).
#[cfg(lazyos_tests)]
pub fn note_for_test(nr: u64, sub: u64) {
    note(nr, sub);
}
