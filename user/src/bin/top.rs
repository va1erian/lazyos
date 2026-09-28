//! `top` (`TOP.ELF`): the native system monitor (issue #144).
//!
//! `top` reads the kernel's read-only system-stats syscall (13) through the
//! typed [`user::sysinfo`] client, renders a compact table — one memory line
//! and the top tasks by CPU ticks — refreshes a few times, then prints the
//! machine-parseable verdict `SYS:TOP:PASS` (or `SYS:TOP:FAIL:<reason>`) and
//! exits.
//!
//! It is the smallest consumer of the new syscall: no Messenger service is
//! needed, so it also proves the syscall works on a plain ring-3 task. The
//! `sysmond` service exposes the same snapshot over Messenger for dashboards.
//!
//! The on-disk name is `TOP.ELF` (8.3-safe: the kernel's FAT reader only
//! resolves short names). `init` starts it once from its manifest, after
//! `sysmond`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::panic::PanicInfo;
use user::messenger;
use user::sys;
use user::sysinfo::{self, Snapshot, TaskRow, TaskState, WaitKind};

/// How many snapshots to render.
const REFRESHES: usize = 3;
/// Ticks to park between refreshes (100 Hz).
const REFRESH_TICKS: u64 = 10;
/// Task rows in the table.
const TOP_N: usize = 6;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    sys::write_str("top: LazyOS system monitor (issue #144)\n");
    match run() {
        Ok(()) => {
            sys::write_str("SYS:TOP:PASS\n");
            sys::exit(0)
        }
        Err(reason) => {
            sys::write_str("SYS:TOP:FAIL:");
            sys::write_str(&reason);
            sys::write_str("\n");
            sys::exit(1)
        }
    }
}

/// Render a few snapshots, sleeping between them.
fn run() -> Result<(), String> {
    for refresh in 1..=REFRESHES {
        let snapshot = sysinfo::snapshot().map_err(|code| format!("snapshot errno {code}"))?;
        render(&snapshot, refresh);
        if refresh < REFRESHES {
            park(REFRESH_TICKS);
        }
    }
    Ok(())
}

/// One frame: the header, the memory line and the top tasks by CPU ticks.
fn render(snapshot: &Snapshot, refresh: usize) {
    sys::write_str(&format!(
        "--- top {refresh}/{REFRESHES} tick={} tasks={} ---\n",
        snapshot.ticks, snapshot.tasks_live
    ));
    sys::write_str(&format!(
        "mem: frames {}/{} live, {} free; slab {}B (peak {}B); heap {}B/{}B\n",
        snapshot.frames_live,
        snapshot.frames_total,
        snapshot.frames_free,
        snapshot.slab_live,
        snapshot.slab_peak,
        snapshot.heap_used,
        snapshot.heap_total,
    ));
    sys::write_str("  PID STATE  CLASS    CPU NAME\n");
    let mut tasks: Vec<TaskRow> = snapshot.live_tasks().copied().collect();
    tasks.sort_by(|left, right| right.cpu_ticks.cmp(&left.cpu_ticks));
    for row in tasks.iter().take(TOP_N) {
        sys::write_str(&format!(
            "  {:>3} {:<6} {:<6} {:>4} {}\n",
            row.pid,
            state_label(row),
            row.class.label(),
            row.cpu_ticks,
            row.name(),
        ));
    }
}

/// `run`, `block/pipe`, `done` — a blocked task's wait kind rides along.
fn state_label(row: &TaskRow) -> String {
    if row.state == TaskState::Blocked && row.wait != WaitKind::None {
        format!("{}/{}", row.state.label(), row.wait.label())
    } else {
        row.state.label().to_string()
    }
}

/// Park for `ticks` by waiting on a private channel pair with a deadline
/// (userspace has no sleep syscall); the pair is closed again so no channel
/// leaks.
fn park(ticks: u64) {
    let deadline = sys::clock() + ticks.max(1);
    if let Ok((probe, peer)) = messenger::create_pair() {
        let mut scratch = [0u8; 16];
        let _ = probe.recv_into(&mut scratch, Some(deadline));
        let _ = probe.close();
        let _ = peer.close();
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
