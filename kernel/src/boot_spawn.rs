//! Which programs the kernel itself spawns at boot: the boot profile's spawn
//! list (demo, services, ABI bench, the console shell) and the helpers the
//! spawn list calls. Split out of `main.rs` (file-size budget); `kernel_main`
//! runs it inline — the order of the spawns is the boot's contract, so the
//! `else` branch there stays the one place that lists them.

use crate::process;
use crate::{ipc, task};

/// Open a program on the OS volume for the loader to stream, as the kernel
/// (root). `None` when it is missing.
pub fn open_program(path: &str) -> Option<process::image::VfsFile> {
    process::image::VfsFile::native(crate::fs::vfs::Id::current(), path).ok()
}

/// Load a program from the OS volume and spawn it as a task named after its
/// file (`/system/bin/init` runs as `init`), if present. Its `argv` is the
/// path then `args`, recorded in the per-task block `spawnv` fills, item for
/// item (no command line is composed or parsed).
pub fn spawn_program(path: &'static str, args: &[&str]) {
    let name = fhs::bin::name(path);
    let bytes = open_program(path);
    boot_phase!("read_{name}");
    let Some(bytes) = bytes else {
        return serial_println!("LazyOS: {path} not found");
    };
    match task::spawn(name, &bytes) {
        Ok(index) => {
            boot_phase!("spawn_{name}");
            let argv: alloc::vec::Vec<&str> =
                core::iter::once(path).chain(args.iter().copied()).collect();
            process::set_task_argv(index, &argv);
            // Every kernel-started program is root, but only `init` may
            // hold the raw input bus (it hands reading to `inputd` alone
            // and publishing to input drivers).
            if path != fhs::bin::INIT {
                drop_input_caps(index);
            }
            // The compositor is latency-sensitive like the kernel mux, and
            // it must take the display grant before any app that would
            // otherwise fall back to owning the screen itself: a strictly
            // higher class makes that ordering deterministic instead of a
            // race the stride scheduler happens to win (issue #338).
            if path == fhs::bin::XUID {
                task::set_priority(index, task::PriorityClass::Interactive);
            }
            serial_println!("LazyOS: spawned {name} as task {index}")
        }
        Err(err) => serial_println!("LazyOS: spawn {name} failed: {err}"),
    }
}

/// The `netdrv` boot arguments: `demo=1` runs the network harness's
/// evidence. `LAZYOS_NET_ARGS` overrides them at build time as a word list
/// (the harness's `--poll` passes `demo=1 irq=poll`). With `netd` present
/// the driver runs only its own ARP self-test: the evidence clients attach to
/// the NIC, and `netd` holds the one attachment.
#[cfg(all(net_demo, not(services_mode)))]
pub fn net_demo_args() -> alloc::vec::Vec<&'static str> {
    const NET_ARGS: &str = match option_env!("LAZYOS_NET_ARGS") {
        Some(args) => args,
        None if cfg!(netd_demo) => "selftest=1",
        None => "demo=1",
    };
    NET_ARGS.split_ascii_whitespace().collect()
}

/// The `netd` boot arguments: `demo=1` runs the network harness's evidence
/// clients, which talk to the harness's host servers. `LAZYOS_NETD_ARGS`
/// overrides them at build time (`run_demo.py --net` passes `demo=0`: an
/// interactive boot runs the stack alone).
#[cfg(all(netd_demo, not(services_mode)))]
pub fn netd_demo_args() -> alloc::vec::Vec<&'static str> {
    const NETD_ARGS: &str = match option_env!("LAZYOS_NETD_ARGS") {
        Some(args) => args,
        None => "demo=1",
    };
    NETD_ARGS.split_ascii_whitespace().collect()
}

/// Load a static Linux-ABI (musl) program and spawn it, if present. The xui
/// app is a `std` binary, so it boots through the Linux path (`spawn_linux`).
#[cfg(xui_app)]
pub fn spawn_linux_program(name: &'static str, path: &str) {
    match open_program(path) {
        Some(bytes) => match task::spawn_linux(name, &bytes, name) {
            Ok(index) => {
                drop_input_caps(index);
                serial_println!("LazyOS: spawned {name} as task {index}")
            }
            Err(err) => serial_println!("LazyOS: spawn {name} failed: {err}"),
        },
        None => serial_println!("LazyOS: {path} not found"),
    }
}

/// Spawn the console shell (issue #254): BusyBox `sh`. Used by the CLI and demo
/// profiles; `logind` starts the login shell the same way (through the
/// `linux:sh` passwd field). With no BusyBox on the image there is no shell
/// (the ad hoc interpreter was retired), so this logs clearly and boots without
/// one rather than crashing.
#[cfg(not(services_mode))]
pub fn spawn_console_shell() {
    match open_program(fhs::bin::BUSYBOX) {
        Some(bytes) => {
            serial_println!("LazyOS: launching busybox sh");
            match task::spawn_linux_args("sh", &bytes, &["sh"]) {
                Ok(index) => {
                    drop_input_caps(index);
                    serial_println!("LazyOS: spawned busybox as task {index}")
                }
                Err(err) => serial_println!("LazyOS: spawn busybox failed: {err}"),
            }
        }
        None => serial_println!(
            "LazyOS: no {} on the image; no console shell (build it with tools/abi/build.py)",
            fhs::bin::BUSYBOX
        ),
    }
}

/// Withhold the raw input bus from a kernel-started program (issue #447):
/// reading every keystroke (`CAP_INPUT_RAW`) and posing as an input device
/// (`CAP_INPUT_SOURCE`) are `init`'s to hand to `inputd` and the input
/// drivers. The xui app (`XAPP`) and the console shell get no more than any
/// other program the kernel starts.
pub fn drop_input_caps(index: usize) {
    ipc::credentials::drop_caps(
        index,
        ipc::credentials::CAP_INPUT_RAW | ipc::credentials::CAP_INPUT_SOURCE,
    );
}
