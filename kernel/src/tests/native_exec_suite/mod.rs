//! `execve` of native LazyOS programs from a Linux task (issue #315): BusyBox
//! `sh` forks a child that `execve`s `top`/`confctl`/..., and the kernel runs
//! the program as a native child of that forked task, parks for it, and exits
//! with its status.
//!
//! The blocking park itself needs a live scheduler, which the harness does
//! not run, so these tests drive the same building blocks in the order
//! `native::try_exec` uses them: `lookup`, `exec_argv`, `spawn`, then the
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

/// The programs the lookup tests expect on the volume, at their real paths
/// (the busybox and rhai files are Linux programs, never native).
const SHIPPED: &[&str] = &[
    fhs::bin::TOP,
    fhs::bin::CONFCTL,
    fhs::bin::MESSENGERCTL,
    fhs::bin::FAULTPROBE,
    fhs::bin::BEEP,
    fhs::bin::MODPLAY,
    fhs::bin::PKGCTL,
    fhs::bin::POWERCTL,
    fhs::bin::BUSYBOX,
    fhs::bin::RHAI,
];

/// Put [`SHIPPED`] into the test VFS as empty 0755 files.
fn install_programs() -> Result<(), String> {
    let id = crate::fs::vfs::Id::current();
    for dir in [fhs::SYSTEM, fhs::SYSTEM_BIN] {
        crate::fs::abi_mkdir(id, dir, 0o755)
            .map_err(|e| format!("mkdir {dir}: {}", e.message()))?;
    }
    for program in SHIPPED {
        crate::fs::abi_create(id, program, 0o755)
            .map_err(|e| format!("create {program}: {}", e.message()))?;
    }
    Ok(())
}

/// Names a shell can type map to native programs in `/system/bin`, byte for
/// byte; everything else is left to the Linux loader (BusyBox applets, real
/// files, Linux programs in `/system/bin`, programs this image lacks).
pub fn lookup_maps_names_to_files() -> Result<(), String> {
    fresh();
    install_programs()?;
    for (path, file) in [
        ("top", fhs::bin::TOP),
        ("/bin/top", fhs::bin::TOP),
        ("/system/bin/top", fhs::bin::TOP),
        ("/usr/bin/confctl", fhs::bin::CONFCTL),
        ("/sbin/msgctl", fhs::bin::MESSENGERCTL),
        ("msgctl", fhs::bin::MESSENGERCTL),
        ("messengerctl", fhs::bin::MESSENGERCTL),
        ("/bin/faultprobe", fhs::bin::FAULTPROBE),
        ("beep", fhs::bin::BEEP),
        ("/usr/bin/beep", fhs::bin::BEEP),
        ("modplay", fhs::bin::MODPLAY),
        ("/usr/bin/modplay", fhs::bin::MODPLAY),
        ("pkgctl", fhs::bin::PKGCTL),
        ("/usr/bin/pkgctl", fhs::bin::PKGCTL),
        ("powerctl", fhs::bin::POWERCTL),
        ("shutdown", fhs::bin::POWERCTL),
        ("/sbin/poweroff", fhs::bin::POWERCTL),
        ("/bin/halt", fhs::bin::POWERCTL),
        ("reboot", fhs::bin::POWERCTL),
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
        "/tmp/reboot",
        "/bin/rebootx",
        // Case-sensitive since F3: the old flat names and other spellings
        // are not found.
        "TOP",
        "top.elf",
        "/TOP.ELF",
        "/faultprb.elf",
        "/beep.elf",
        "/system/bin/TOP",
        "/system/bin/top.elf",
        "/system/bin/msgctl",
        "/system/bin/reboot",
        // Linux programs in /system/bin, and natives this image lacks.
        "/system/bin/busybox",
        "rhai",
        "/system/bin/rhai",
        "ping",
        fhs::bin::PING,
    ] {
        check!(
            native::lookup(path).is_none(),
            "lookup({path:?}) = {:?}, expected no native program",
            native::lookup(path)
        );
    }
    for (path, preset) in [
        ("shutdown", "poweroff"),
        ("/sbin/poweroff", "poweroff"),
        ("halt", "poweroff"),
        ("/usr/sbin/reboot", "reboot"),
        ("powerctl", ""),
        (fhs::bin::POWERCTL, ""),
        ("top", ""),
        ("msgctl", ""),
    ] {
        check!(
            native::preset_args(path) == preset,
            "preset_args({path:?}) = {:?}, expected {preset:?}",
            native::preset_args(path)
        );
    }
    Ok(())
}

/// A real file named like a native program wins over the short name, so the
/// alias never hides something the user installed.
pub fn lookup_never_shadows_real_files() -> Result<(), String> {
    fresh();
    install_programs()?;
    let id = crate::fs::vfs::Id::current();
    check!(
        native::lookup("/usr/bin/top") == Some(fhs::bin::TOP),
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
        native::lookup("/bin/top") == Some(fhs::bin::TOP),
        "the other search directories stopped resolving"
    );

    Ok(())
}

/// The `argv` an `execve`d native program receives is the caller's vector
/// item for item: an argument with spaces stays one item (it used to be
/// joined and re-split), the NUL terminators are dropped, an alias's preset
/// follows `argv[0]`, hostile bytes cannot panic, and a list over `spawnv`'s
/// limits is refused.
pub fn exec_argv_keeps_items() -> Result<(), String> {
    let argv =
        |items: &[&[u8]]| -> Vec<Vec<u8>> { items.iter().map(|item| item.to_vec()).collect() };
    let strings = |items: &[&str]| -> Option<Vec<String>> {
        Some(items.iter().map(|item| String::from(*item)).collect())
    };
    check!(
        native::exec_argv("top", &argv(&[b"top\0"])) == strings(&["top"]),
        "no arguments should give argv[0] alone"
    );
    check!(
        native::exec_argv(
            "confctl",
            &argv(&[b"confctl\0", b"get\0", b"a b/c d\0", b"\0"])
        ) == strings(&["confctl", "get", "a b/c d", ""]),
        "arguments with spaces or empty were split or dropped"
    );
    check!(
        native::exec_argv("/bin/reboot", &argv(&[b"reboot\0", b"now please\0"]))
            == strings(&["reboot", "reboot", "now please"]),
        "the alias preset was not inserted after argv[0]"
    );
    check!(
        native::exec_argv("/system/bin/top", &[]) == strings(&["/system/bin/top"]),
        "an empty argv did not get the path as argv[0]"
    );
    check!(
        native::exec_argv("x", &argv(&[b"x", b"no-nul"])) == strings(&["x", "no-nul"]),
        "an entry without a terminator was mangled"
    );
    let lossy = native::exec_argv("x", &argv(&[b"x\0", b"\xff\xfe\0"]));
    check!(
        lossy.as_ref().is_some_and(|items| items.len() == 2),
        "invalid UTF-8 must not fail the exec: {lossy:?}"
    );
    let huge = vec![b'a'; 3000];
    check!(
        native::exec_argv("x", &argv(&[b"x\0", &huge, &huge])).is_none(),
        "6000 bytes of arguments were accepted"
    );
    let many: Vec<Vec<u8>> = (0..65).map(|_| b"a\0".to_vec()).collect();
    check!(
        native::exec_argv("x", &many).is_none(),
        "65 arguments were accepted"
    );
    Ok(())
}

/// End to end through `execve`'s building blocks: an argument with spaces
/// reaches the spawned native program as one `argv` item through syscall 9.
pub fn exec_argv_with_spaces_reaches_program() -> Result<(), String> {
    fresh();
    let sh = shell()?;
    let argv = native::exec_argv(
        "/bin/confctl",
        &[
            b"confctl\0".to_vec(),
            b"set\0".to_vec(),
            b"a key\0".to_vec(),
            b"two  spaces \0".to_vec(),
        ],
    )
    .ok_or("exec_argv refused a small list")?;
    let child = native::spawn(fhs::bin::CONFCTL, &service_suite::minimal_elf(), &argv)
        .map_err(|e| format!("spawn errno {e}"))?;
    task::harness::switch_current(child);
    let mut buf = [0u8; 64];
    let len = process::dispatch_for_test(9, buf.as_mut_ptr() as u64, buf.len() as u64, 0);
    task::harness::switch_current(sh);
    let want = b"confctl\0set\0a key\0two  spaces \0";
    check!(
        len == want.len() as u64 && &buf[..want.len()] == want,
        "argv block was {len} bytes: {:?}",
        &buf[..(len as usize).min(64)]
    );
    task::harness::finish(child, 0);
    check!(task::reap_child_slot(child) == Some(0), "child not reaped");
    task::harness::switch_current(task::KERNEL_TASK);
    task::harness::reset();
    check!(
        process::task_args_live_for_test() == 0,
        "the reaped program's argv block was kept"
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
    ("native_exec_exec_argv_keeps_items", exec_argv_keeps_items),
    (
        "native_exec_argv_with_spaces_reaches_program",
        exec_argv_with_spaces_reaches_program,
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
