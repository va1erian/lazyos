//! `messengerctl` (`MSGCTL.ELF`): render the Messenger fabric snapshot and
//! browse the name registry, the service supervisor, health and the event log
//! (issues #70, #89 and #93). The image name is 8.3 because the kernel's FAT
//! reader only resolves short names.
//!
//! Calls the native `messenger` syscall's `stats` op with a snapshot-sized
//! buffer, so the kernel returns the versioned `FabricStats` block (ABI v3),
//! and prints it as a small table grouped by subsystem: services/channels,
//! messages, buffers, audit, and per-slot usage. It then offers the registry
//! commands `list` and `resolve <name>`, the supervisor commands `services`
//! and `health`, `sessions` (the `logind` session table, issue #101), the
//! `log`/`log tail`/`log verify` commands, the shell-integration commands
//! `mime <path>` and `open <path>` (issue #116), and the app commands
//! `apps` and `launch <app> [args]` (issue #158), typed at the prompt (native
//! programs do not receive argv; the tool is interactive like `sh`).
//!
//! When a topics broker is reachable (boot the demo with both
//! `LAZYOS_MESSENGERD=1` and `LAZYOS_MESSENGERCTL=1`) the tool also runs a
//! boot-time topic conformance self-test and prints machine-parseable serial
//! markers (`TOPIC:FANOUT:PASS`, `TOPIC:WILDCARD:PASS`, `TOPIC:RETAINED:PASS`,
//! `TOPIC:DROP:PASS`, `TOPIC:QOS:PASS`, `TOPIC:UNSUB:PASS`), so a headless
//! `qemu_session.py` run proves the pub/sub path end to end. The interactive
//! commands `topics` and `tail <filter> [count]` inspect and stream (issue
//! #92).
//!
//! With services running the boot self-test also exercises the app registry and
//! launch path (issue #158): `MSGCTL:APPS:PASS`, `MSGCTL:LAUNCH:PASS` and the
//! foreign-session `MSGCTL:LAUNCH:DENIED:PASS` probe. The probe runs in a
//! short-lived child (`MSGCTL.ELF probe`, issue #177): the kernel never lets a
//! task widen its own credentials back up, so if the console task dropped its
//! own privilege to run the probe it could never regain it, and every command
//! typed afterward would run as the probe's uid. A disposable child can drop
//! to the probe identity and exit; the console keeps its own credentials.
//!
//! Boot it with `LAZYOS_MESSENGERCTL=1` (see the kernel build script): the
//! demo then runs this program in the hello window. With `LAZYOS_SERVICES=1`
//! the supervisor's services provide targets for the new commands.

#![no_std]
#![no_main]

extern crate alloc;

#[path = "messengerctl/clients.rs"]
mod clients;
#[path = "messengerctl/commands.rs"]
mod commands;
#[path = "messengerctl/names.rs"]
mod names;
#[path = "messengerctl/render.rs"]
mod render;
#[path = "messengerctl/selftest.rs"]
mod selftest;
#[path = "messengerctl/supervisor.rs"]
mod supervisor;
#[path = "messengerctl/system.rs"]
mod system;
#[path = "messengerctl/topic_tests.rs"]
mod topic_tests;
#[path = "messengerctl/topics.rs"]
mod topics;

use core::panic::PanicInfo;
use user::messenger;
use user::sys;

use commands::{commands, report};
use render::print_report;
use selftest::{keyd_selftest, run_forbidden_publish_probe, topic_selftest};
use supervisor::{app_selftest, probe_role};

#[no_mangle]
pub extern "C" fn _start() -> ! {
    if is_probe_role() {
        probe_role();
    }
    // `TOPIC:SECURITY`'s negative test spawns this same binary under a
    // demoted, non-root credential (see `selftest_security`): credentials
    // cannot be restored once dropped, so the probe runs as a throwaway
    // child rather than in this (normally uid-0) process. When invoked this
    // way, run only the probe and exit; the demo/registry flow below never
    // starts.
    if service_arg_is("probe", "forbidden-publish") {
        run_forbidden_publish_probe();
    }
    sys::write_str("messengerctl: Messenger fabric snapshot\n");
    match messenger::fabric_stats() {
        Ok(stats) => print_report(&stats),
        Err(error) => report(error.message()),
    }
    topic_selftest();
    keyd_selftest();
    app_selftest();
    commands()
}

/// Read this task's service argument (see [`probe_role`]): `true` for a
/// `MSGCTL.ELF probe` child, `false` for the ordinary console tool.
fn is_probe_role() -> bool {
    let mut buffer = [0u8; 16];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    core::str::from_utf8(&buffer[..len]).unwrap_or("").trim() == "probe"
}

/// Whether this service's argument string carries `key=value`.
fn service_arg_is(key: &str, value: &str) -> bool {
    let mut buffer = [0u8; 128];
    let len = sys::service_args(&mut buffer).min(buffer.len());
    let text = core::str::from_utf8(&buffer[..len]).unwrap_or("");
    text.split_whitespace().any(|part| {
        part.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
            == Some(value)
    })
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    sys::exit(1)
}
