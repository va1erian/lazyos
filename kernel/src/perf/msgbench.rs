//! Start the cross-process Messenger benchmark behind `PERF:msg_rt` and
//! `PERF:msg_tput` (docs/performance-plan.md P6 step 0).
//!
//! `/system/bin/msgbench` measures itself: it starts its own server child and
//! prints its `PERF:` lines in the kernel's format, so the kernel's only job
//! is to start it once, like a boot spawn. It runs as root without a label,
//! as every kernel-started program does.

use crate::{fs, process, task};

/// Spawn the benchmark client; it reports, and fails, on its own.
pub fn start() {
    // The loader reads the volume and takes the task table: spin locks that
    // syscalls take with interrupts off (the #382 rule), so not from the
    // kernel task's interrupts-on loop.
    x86_64::instructions::interrupts::without_interrupts(|| {
        let path = fhs::bin::MSGBENCH;
        let Ok(file) = process::image::VfsFile::native(fs::vfs::Id::current(), path) else {
            crate::serial_println!("PERF:msg_rt:SKIP {path} missing");
            return;
        };
        match task::spawn(fhs::bin::name(path), &file) {
            Ok(slot) => process::set_task_argv(slot, &[path]),
            Err(error) => crate::serial_println!("PERF:msg_rt:SKIP spawn failed: {error}"),
        }
    });
}
