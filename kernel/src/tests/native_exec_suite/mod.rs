//! `execve` of native LazyOS programs from a Linux task (issue #315): BusyBox
//! `sh` forks a child that `execve`s `top`/`confctl`/..., and the kernel runs
//! the program as a native child of that forked task, parks for it, and exits
//! with its status.
//!
//! The blocking park itself needs a live scheduler, which the harness does
//! not run, so these tests drive the same building blocks in the order
//! `native::try_exec` uses them: `lookup`, `args_line`, `spawn`, then the
//! non-blocking half of `wait_for` (`reap_child_slot`) once the harness
//! finishes the child. The end-to-end path (parking, `sh` reporting the
//! status, `&`) is covered by the screenshot sessions.

use super::*;
use crate::process::linux::native;

mod lifecycle;
mod soak;
mod stdin;

use lifecycle::*;
use soak::*;
use stdin::*;

/// `-errno` as the syscall ABI returns it.
fn failed(code: i64) -> u64 {
    (code as u64).wrapping_neg()
}

/// Bring-up: an ABI VFS for the shadowing test, the kernel task current and
/// every other slot free.
fn fresh() {
    task::register_kernel();
    task::harness::reset();
    task::harness::switch_current(task::KERNEL_TASK);
    for slot in 0..task::MAX_TASKS {
        crate::ipc::credentials::reset_for_task(slot);
    }
    crate::fs::install_abi_ramfs_for_test();
}

/// A shell-like task: a native child of the kernel task that becomes the
/// current task, standing in for the forked child of `sh`.
fn shell() -> Result<usize, String> {
    let slot = task::spawn_child("shell", &service_suite::minimal_elf()).map_err(to_string)?;
    task::harness::switch_current(slot);
    Ok(slot)
}

/// Names a shell can type map to boot-volume files; everything else is left
/// to the Linux loader (BusyBox applets, real files).
pub fn lookup_maps_names_to_files() -> Result<(), String> {
    fresh();
    for (path, file) in [
        ("top", "TOP.ELF"),
        ("/bin/top", "TOP.ELF"),
        ("/usr/bin/confctl", "CONFCTL.ELF"),
        ("/sbin/msgctl", "MSGCTL.ELF"),
        ("messengerctl", "MSGCTL.ELF"),
        ("/bin/faultprobe", "FAULTPRB.ELF"),
        ("/TOP.ELF", "TOP.ELF"),
        ("top.elf", "TOP.ELF"),
        ("/faultprb.elf", "FAULTPRB.ELF"),
        ("beep", "BEEP.ELF"),
        ("/usr/bin/beep", "BEEP.ELF"),
        ("/beep.elf", "BEEP.ELF"),
    ] {
        check!(
            native::lookup(path) == Some(file),
            "lookup({path:?}) = {:?}, expected {file:?}",
            native::lookup(path)
        );
    }
    for path in [
        "",
        "/",
        "/bin/",
        "ls",
        "/bin/sh",
        "busybox",
        "/bin/topx",
        "/bin/TOP",
        "/tmp/top",
        "/opt/top",
        "/tmp/bin/top",
        "/mybin/top",
        "/usr/local/top",
        "/bin/top/x",
        "/etc/TOP.ELF",
        "SUPER.ELF",
        "/bin/init",
        "/tmp/beep",
        "/bin/beepx",
    ] {
        check!(
            native::lookup(path).is_none(),
            "lookup({path:?}) = {:?}, expected no native program",
            native::lookup(path)
        );
    }
    Ok(())
}

/// A real file named like a native program wins over the short name, so the
/// alias never hides something the user installed.
pub fn lookup_never_shadows_real_files() -> Result<(), String> {
    fresh();
    let id = crate::fs::vfs::Id::current();
    check!(
        native::lookup("/usr/bin/top") == Some("TOP.ELF"),
        "an unclaimed name in a bin directory should alias"
    );
    crate::fs::abi_mkdir(id, "/usr", 0o755).map_err(|e| format!("mkdir /usr: {}", e.message()))?;
    crate::fs::abi_mkdir(id, "/usr/bin", 0o755)
        .map_err(|e| format!("mkdir /usr/bin: {}", e.message()))?;
    crate::fs::abi_create(id, "/usr/bin/top", 0o755)
        .map_err(|e| format!("create: {}", e.message()))?;
    check!(
        native::lookup("/usr/bin/top").is_none(),
        "a real file was shadowed by the native alias"
    );
    check!(
        native::lookup("/bin/top") == Some("TOP.ELF"),
        "the other search directories stopped resolving"
    );
    Ok(())
}

/// `argv[1..]` becomes one space-joined string; the program name and the NUL
/// terminators are not part of it, hostile bytes cannot panic, and an
/// oversized list is refused.
pub fn args_line_joins_and_bounds() -> Result<(), String> {
    let argv =
        |items: &[&[u8]]| -> Vec<Vec<u8>> { items.iter().map(|item| item.to_vec()).collect() };
    check!(
        native::args_line(&argv(&[b"top\0"])).as_deref() == Some(""),
        "no arguments should give an empty line"
    );
    check!(
        native::args_line(&argv(&[b"confctl\0", b"get\0", b"a/b\0"])).as_deref() == Some("get a/b"),
        "arguments were not joined"
    );
    check!(
        native::args_line(&argv(&[b"x", b"no-nul"])).as_deref() == Some("no-nul"),
        "an entry without a terminator was mangled"
    );
    let lossy = native::args_line(&argv(&[b"x\0", b"\xff\xfe\0"]));
    check!(lossy.is_some(), "invalid UTF-8 must not fail the exec");
    let huge = vec![b'a'; 3000];
    check!(
        native::args_line(&argv(&[b"x\0", &huge, &huge])).is_none(),
        "6000 bytes of arguments were accepted"
    );
    Ok(())
}

pub(super) const CASES: &[(&str, Test)] = &[
    (
        "native_exec_lookup_maps_names_to_files",
        lookup_maps_names_to_files,
    ),
    (
        "native_exec_lookup_never_shadows_real_files",
        lookup_never_shadows_real_files,
    ),
    (
        "native_exec_args_line_joins_and_bounds",
        args_line_joins_and_bounds,
    ),
    (
        "native_exec_spawn_inherits_fds_args_and_reports_status",
        spawn_inherits_fds_args_and_reports_status,
    ),
    (
        "native_exec_reap_is_specific_to_the_child",
        reap_is_specific_to_the_child,
    ),
    (
        "native_exec_failures_report_errno_without_leaking",
        failures_report_errno_without_leaking,
    ),
    (
        "native_exec_background_program_is_reaped_through_the_shell",
        background_program_is_reaped_through_the_shell,
    ),
    (
        "native_exec_read_char_follows_redirected_stdin",
        read_char_follows_redirected_stdin,
    ),
    (
        "native_exec_read_char_leaves_seqpacket_messages_intact",
        read_char_leaves_seqpacket_messages_intact,
    ),
    ("native_exec_soak_spawn_exit_cycles", soak_spawn_exit_cycles),
];
