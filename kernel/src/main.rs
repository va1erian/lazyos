//! LazyOS kernel entry point.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
// Every unsafe block must justify itself in place (issue #124): the kernel
// has no runtime backstop for a bad unsafe block, and a repo-wide audit is
// only as durable as the lint that stops the next one from going undocumented.
#![warn(clippy::undocumented_unsafe_blocks)]

extern crate alloc;

#[macro_use]
mod macros;
#[macro_use]
mod boot_trace;

mod arch;
mod block;
mod console;
mod cursor;
mod dev;
mod display;
mod entropy;
mod error;
mod font;
mod fs;
mod gfx;
#[allow(dead_code)]
mod gfxlib;
mod input;
#[allow(dead_code)] // Kernel-side fabric; the native syscall surface landed in #69.
mod ipc;
mod mem;
mod mux;
mod process;
mod quota;
mod serial;
#[allow(dead_code)]
mod skia;
mod surface;
mod sysinfo;
mod task;
#[cfg(lazyos_tests)]
mod tests;
mod text;
mod user_ptr;
mod wallclock;

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::info::Optional;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;

/// Request a full physical-memory mapping so the kernel can manage page tables.
const CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &CONFIG);

// In test builds `tests::run()` diverges before the normal boot path.
#[cfg_attr(lazyos_tests, allow(unreachable_code))]
fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    serial::init();
    serial_println!("LazyOS: kernel entered");
    boot_phase!("kernel_entered");

    let (base, info) = match &mut boot_info.framebuffer {
        Optional::Some(framebuffer) => {
            let base = framebuffer.buffer_mut().as_mut_ptr() as usize;
            (base, framebuffer.info())
        }
        Optional::None => {
            serial_println!("LazyOS: no framebuffer; halting");
            halt();
        }
    };

    console::init(base, info);
    serial_println!(
        "LazyOS: framebuffer {}x{} {:?}",
        info.width,
        info.height,
        info.pixel_format
    );
    // The display grant (issue #113) needs the framebuffer geometry to size a
    // compositor's screen buffer; the mux itself keeps using the console. This
    // is recorded before the test hook so the kernel suite sees it too.
    display::init(info.width, info.height, info.stride, info.bytes_per_pixel);

    boot_phase!("console_ready");
    // `mem::init` keeps the boot info borrowed, so read the ramdisk hand-off first.
    let (ramdisk_addr, ramdisk_len) = (boot_info.ramdisk_addr, boot_info.ramdisk_len);
    mem::init(boot_info);
    boot_phase!("mem_ready");

    // Device core (issue #239): enumerate platform + PCI devices, attach the
    // in-kernel drivers (ATA, legacy virtio-blk) and print the `DEV:ENUM` line.
    // Idempotent, so `fs::init`'s later block probe is a no-op.
    dev::init();

    // Kernel test mode (issue #62): run the in-kernel suite and halt instead of
    // booting the demo. Compiled in only with `LAZYOS_TESTS=1`.
    #[cfg(lazyos_tests)]
    tests::run();

    // Driver class rules (issue #481), before `init` can start any driver.
    dev::policy::install_boot_policy();

    // Issue #5: a bootloader ramdisk (a FAT image) is a fallback block device,
    // so the OS still boots with no ATA/virtio disk attached. Probing the real
    // disks first keeps them ahead of it in the mount order.
    if let Optional::Some(addr) = ramdisk_addr {
        block::init();
        if block::mem::register_ramdisk(addr, ramdisk_len) {
            serial_println!("block: ramdisk registered ({} bytes)", ramdisk_len);
        }
    }

    if fs::init() {
        serial_println!("LazyOS: FAT16 filesystem mounted");
    } else {
        serial_println!("LazyOS: no filesystem found");
    }

    boot_phase!("fs_ready");
    // Descriptor tables, interrupts (PIC/PIT), and the PS/2 mouse.
    arch::init();
    boot_phase!("arch_ready");
    // Interrupt vectors and the device syscall are live: print their evidence.
    dev::selfcheck();
    input::mouse::set_bounds(info.width as i32, info.height as i32);

    // Register the kernel (multiplexer) task and spawn the demo programs, the
    // injected Linux fixture, or a BusyBox shell.
    task::register_kernel();

    // Issue #69: create the bootstrap Messenger channel pair. The service end
    // stays kernel-side (the `messengerd` stub) and the first userspace task to
    // call the `bootstrap` op claims the client end in its own handle table.
    match ipc::syscalls::bootstrap::create() {
        Ok(()) => serial_println!("msg: bootstrap channel ready"),
        Err(error) => serial_println!("msg: bootstrap channel failed: {error}"),
    }
    // Issue #89: publish the bootstrap listener under the registry's
    // well-known name, so any task can resolve the daemon endpoint.
    match ipc::syscalls::bootstrap::publish("os.lazy.messenger.registry") {
        Ok(()) => serial_println!("msg: registry name os.lazy.messenger.registry published"),
        Err(error) => serial_println!("msg: registry name publish failed: {error}"),
    }
    // The ABI bench's BusyBox row (`LAZYOS_BUSYBOX_TEST=1`) runs one command
    // and prints the marker, so the bench can classify it from serial. Every
    // other image boots its normal profile; BusyBox is embedded so the profiles
    // can host it (the console shell, `logind`, the desktop Terminal), not to
    // replace the whole session.
    if cfg!(busybox_test) {
        match fs::read(fhs::boot::BUSYBOX) {
            Some(bytes) => {
                serial_println!("LazyOS: launching busybox sh (bench)");
                // `df` and `mount` list what `/proc/mounts` says; with a data
                // disk attached the bench requires `/data` in both (#348).
                // Then `cd /data` and back (#365): `pwd -P` asks the kernel,
                // `ls` is an exec'd child that must inherit the directory,
                // and the redirection writes relative to it. (No command
                // substitution: the bench judges the shell's own output.)
                let script = "echo ABI:busybox:PASS; df; mount; \
                    cd /data && echo ABI:busybox:CWD && pwd -P && \
                    echo probe > cwdprobe && echo ABI:busybox:LS && ls && \
                    cd .. && echo ABI:busybox:CWD2 && pwd -P && \
                    echo ABI:busybox:LS2 && ls; \
                    rm -f /data/cwdprobe; echo ABI:busybox:END";
                match task::spawn_linux_args("sh", &bytes, &["sh", "-c", script]) {
                    Ok(index) => serial_println!("LazyOS: spawned busybox as task {index}"),
                    Err(err) => serial_println!("ABI:busybox:FAIL:{err}"),
                }
            }
            None => serial_println!("ABI:busybox:FAIL:no {} on image", fhs::boot::BUSYBOX),
        }
    } else if let Some(bytes) = fs::read(fhs::boot::INIT_ELF) {
        // The ABI bench's injected fixture owns the boot. Checked before the
        // demo profile so a stray `BUSYBOX` on an image cannot shadow a fixture.
        serial_println!("ABI:INIT:START");
        match task::spawn_linux("init", &bytes, "init") {
            Ok(index) => serial_println!("LazyOS: spawned init as task {index}"),
            Err(err) => serial_println!("ABI:INIT:FAIL:{err}"),
        }
    } else {
        // Issue #93: `LAZYOS_SERVICES=1` boots the userspace supervisor
        // (`SUPER.ELF`). `init` starts the platform services from its manifest
        // (messengerd, logd, healthd, the crash-test service), so the kernel
        // does not spawn them itself; `init` owns supervision, restart policy
        // and the service event log from here on. `SUPER.ELF` (not `INIT.ELF`)
        // keeps the ABI bench's fixture hook above untouched.
        #[cfg(services_mode)]
        spawn_program("init", fhs::boot::SUPER_ELF);

        // With `LAZYOS_MESSENGERCTL=1` too, the fabric tool also boots, so a
        // scripted session can query the new `services`/`health`/`log`
        // commands against the running supervisor (issue #93).
        #[cfg(all(services_mode, messengerctl_demo))]
        spawn_program("messengerctl", fhs::boot::MSGCTL_ELF);

        // Issue #89: `LAZYOS_MESSENGERD=1` starts the registry daemon before
        // the demo programs. It claims the bootstrap channel and serves name
        // requests for the life of the system. The on-disk name is 8.3-safe
        // (`MSGRD.ELF`: the kernel's FAT reader has no long-name support).
        #[cfg(all(messengerd_service, not(services_mode)))]
        spawn_program("messengerd", fhs::boot::MSGRD_ELF);

        // `LAZYOS_MESSENGERCTL=1` swaps the hello window for the fabric
        // snapshot tool (issue #70); the default demo is unchanged. The file
        // name is 8.3: the kernel FAT reader has no long-name support.
        #[cfg(all(messengerctl_demo, not(services_mode)))]
        spawn_program("messengerctl", fhs::boot::MSGCTL_ELF);
        #[cfg(all(not(messengerctl_demo), not(services_mode), not(cli_mode)))]
        spawn_program("hello", fhs::boot::HELLO_ELF);
        #[cfg(not(services_mode))]
        spawn_console_shell();

        // `LAZYOS_SOUND=1` boots the virtio-sound driver directly when there is
        // no supervisor to start it (docs/driver-plan.md D6). `sndd` plays a
        // test tone with `demo=1`, which the sound harness records.
        #[cfg(all(sound_demo, not(services_mode)))]
        spawn_sound_demo();

        // `LAZYOS_NET=1` boots the virtio-net driver directly when there is no
        // supervisor to start it (docs/networking-plan.md N1). `netdrv` runs
        // its ARP self-test and the `nicctl` evidence clients with `demo=1`.
        #[cfg(all(net_demo, not(services_mode)))]
        spawn_net_demo();
        // `LAZYOS_NETD=1` adds the stack service on top of the driver
        // (docs/networking-plan.md N2); it finds the driver by name and retries,
        // so the order does not matter.
        #[cfg(all(netd_demo, not(services_mode)))]
        spawn_netd_demo();

        // Issue #113: `LAZYOS_XUID=1` boots the userspace compositor (`XUID.ELF`)
        // and two instances of the display-protocol demo app (`XDEMO.ELF`).
        // `xuid` binds the display grant, so the mux stops painting and the
        // screen shows the composited windows instead; two clients prove the
        // protocol routes focus per surface. The default demo is untouched
        // without the flag.
        //
        // Issue #114: with `LAZYOS_XUI_APP` set too, the app takes the place of
        // the `xuid` session: it binds the display grant itself (the M0-M2
        // milestones drive the kernel input queue and the screen buffer
        // directly), so the two cannot run together. The `xuid` + `xdemo` demo
        // is unchanged when only `LAZYOS_XUID=1` is set.
        //
        // Issue #168: with `LAZYOS_XUI_CLIENT=1` as well, the app runs as a
        // `xuid` client instead: the compositor owns the grant and the app
        // gets a decorated window over `os.lazy.display.v1`. No `xdemo` is
        // spawned, so the app is the first (and only) surface and is laid out
        // at the top-left corner.
        //
        // Issues #215/#216: with `LAZYOS_XUI_APPS` too (`xui_desktop`), only
        // `xuid` boots here; `init` launches the embedded apps from its
        // registry, so a desktop session runs several of them (Terminal,
        // System Monitor, ...) side by side.
        #[cfg(all(xuid_demo, not(xui_app), not(xui_desktop)))]
        spawn_program("xuid", fhs::boot::XUID_ELF);
        #[cfg(all(xuid_demo, not(xui_app), not(xui_desktop)))]
        spawn_program("xdemo", fhs::boot::XDEMO_ELF);
        #[cfg(all(xuid_demo, not(xui_app), not(xui_desktop)))]
        spawn_program("xdemo", fhs::boot::XDEMO_ELF);
        #[cfg(all(xuid_demo, xui_desktop, not(xui_app)))]
        spawn_program("xuid", fhs::boot::XUID_ELF);
        #[cfg(all(xui_app, not(xui_client)))]
        spawn_linux_program("xapp", fhs::boot::XAPP_ELF);
        #[cfg(xui_client)]
        spawn_program("xuid", fhs::boot::XUID_ELF);
        #[cfg(xui_client)]
        spawn_linux_program("xapp", fhs::boot::XAPP_ELF);

        // Issue #145: the drag & drop demo pair. `dragdemo` is a launcher that
        // starts a drag source and a drop target child, so a scripted session
        // can drag a typed payload from one surface to the other (or cancel it
        // with Escape) and grep the `DND:*` evidence markers from serial. It
        // also starts `clipboardd` (`CLIPD.ELF`) when the supervisor is not
        // running, since the token transfer needs the clipboard service. The
        // xui app owns the display grant, so the demo skips that image; with
        // 64 task slots (issue #204) it fits next to the services too.
        #[cfg(all(xuid_demo, not(xui_app), not(xui_desktop)))]
        spawn_program("dragdemo", fhs::boot::DRAGDMO_ELF);

        // Issue #167: the shell-protocol evidence client. The
        // `LAZYOS_SHELLPROBE=1` demo hook keeps the default `xuid` sessions
        // untouched; when set it boots `shellprobe`, which creates the desktop
        // surface, subscribes to the shell events, and logs the
        // `SHELLPROBE:*:PASS` markers.
        #[cfg(all(xuid_demo, shellprobe_demo, not(xui_app), not(xui_desktop)))]
        spawn_program("shellprobe", fhs::boot::SHELLPRB_ELF);
    }

    let stats = mem::frame_stats();
    serial_println!(
        "mem: live frames {} (allocated {} freed {}), {} free of {}",
        stats.live(),
        stats.allocated,
        stats.freed,
        stats.free,
        stats.total
    );

    boot_phase!("tasks_spawned");
    task::start();
    serial_println!("LazyOS: scheduler started (Tab switches focus)");
    x86_64::instructions::interrupts::enable();

    // The kernel task becomes the terminal multiplexer.
    mux::run();
}

/// Load a program from the FAT disk and spawn it as a task, if present.
fn spawn_program(name: &'static str, path: &str) {
    let bytes = fs::read(path);
    boot_phase!("read_{name}");
    match bytes {
        Some(bytes) => match task::spawn(name, &bytes) {
            Ok(index) => {
                boot_phase!("spawn_{name}");
                // Every kernel-started program is root, but only `init` may
                // hold the raw input bus (it hands reading to `inputd` alone
                // and publishing to input drivers).
                if name != "init" {
                    ipc::credentials::drop_caps(
                        index,
                        ipc::credentials::CAP_INPUT_RAW | ipc::credentials::CAP_INPUT_SOURCE,
                    );
                }
                // The compositor is latency-sensitive like the kernel mux, and
                // it must take the display grant before any app that would
                // otherwise fall back to owning the screen itself: a strictly
                // higher class makes that ordering deterministic instead of a
                // race the stride scheduler happens to win (issue #338).
                if name == "xuid" {
                    task::set_priority(index, task::PriorityClass::Interactive);
                }
                serial_println!("LazyOS: spawned {name} as task {index}")
            }
            Err(err) => serial_println!("LazyOS: spawn {name} failed: {err}"),
        },
        None => serial_println!("LazyOS: {path} not found"),
    }
}

/// Boot `sndd` with `demo=1` (kernel-spawned tasks have no argument string
/// otherwise), so a scripted boot plays the harness's test tone.
#[cfg(all(sound_demo, not(services_mode)))]
fn spawn_sound_demo() {
    let Some(bytes) = fs::read(fhs::boot::SNDD_ELF) else {
        serial_println!("LazyOS: {} not found", fhs::boot::SNDD_ELF);
        return;
    };
    match task::spawn("sndd", &bytes) {
        Ok(index) => {
            process::set_service_args(index, b"demo=1");
            serial_println!("LazyOS: spawned sndd as task {index}");
        }
        Err(err) => serial_println!("LazyOS: spawn sndd failed: {err}"),
    }
}

/// Boot `netdrv` with `demo=1` (kernel-spawned tasks have no argument string
/// otherwise), so a scripted boot runs the network harness's evidence.
#[cfg(all(net_demo, not(services_mode)))]
fn spawn_net_demo() {
    // `LAZYOS_NET_ARGS` overrides the argument string at build time (the
    // network harness's `--poll` passes `demo=1 irq=poll`).
    //
    // With `netd` present the driver runs only its own ARP self-test: the
    // evidence clients attach to the NIC, and `netd` holds the one attachment.
    const NET_ARGS: &str = match option_env!("LAZYOS_NET_ARGS") {
        Some(args) => args,
        None if cfg!(netd_demo) => "selftest=1",
        None => "demo=1",
    };
    let Some(bytes) = fs::read(fhs::boot::NETDRV_ELF) else {
        serial_println!("LazyOS: {} not found", fhs::boot::NETDRV_ELF);
        return;
    };
    match task::spawn("netdrv", &bytes) {
        Ok(index) => {
            process::set_service_args(index, NET_ARGS.as_bytes());
            serial_println!("LazyOS: spawned netdrv as task {index}");
        }
        Err(err) => serial_println!("LazyOS: spawn netdrv failed: {err}"),
    }
}

/// Boot `netd` with `demo=1` so a scripted boot runs `netctl`, `ping`, the
/// hostile-input probe and the soak as real clients.
#[cfg(all(netd_demo, not(services_mode)))]
fn spawn_netd_demo() {
    let Some(bytes) = fs::read(fhs::boot::NETD_ELF) else {
        serial_println!("LazyOS: {} not found", fhs::boot::NETD_ELF);
        return;
    };
    match task::spawn("netd", &bytes) {
        Ok(index) => {
            process::set_service_args(index, b"demo=1");
            serial_println!("LazyOS: spawned netd as task {index}");
        }
        Err(err) => serial_println!("LazyOS: spawn netd failed: {err}"),
    }
}

/// Load a static Linux-ABI (musl) program and spawn it, if present. The xui
/// app is a `std` binary, so it boots through the Linux path (`spawn_linux`).
#[cfg(xui_app)]
fn spawn_linux_program(name: &'static str, path: &str) {
    match fs::read(path) {
        Some(bytes) => match task::spawn_linux(name, &bytes, name) {
            Ok(index) => serial_println!("LazyOS: spawned {name} as task {index}"),
            Err(err) => serial_println!("LazyOS: spawn {name} failed: {err}"),
        },
        None => serial_println!("LazyOS: {path} not found"),
    }
}

/// Spawn the console shell (issue #254): BusyBox `sh`. Used by the CLI and demo
/// profiles; `logind` starts the login shell the same way (through the
/// `linux:sh` passwd field). With no `BUSYBOX` on the image there is no shell
/// (the ad hoc interpreter was retired), so this logs clearly and boots without
/// one rather than crashing.
#[cfg(not(services_mode))]
fn spawn_console_shell() {
    match fs::read(fhs::boot::BUSYBOX) {
        Some(bytes) => {
            serial_println!("LazyOS: launching busybox sh");
            match task::spawn_linux_args("sh", &bytes, &["sh"]) {
                Ok(index) => serial_println!("LazyOS: spawned busybox as task {index}"),
                Err(err) => serial_println!("LazyOS: spawn busybox failed: {err}"),
            }
        }
        None => serial_println!(
            "LazyOS: no {} on the image; no console shell (build it with tools/abi/build.py)",
            fhs::boot::BUSYBOX
        ),
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("LazyOS PANIC: {}", info);
    halt();
}

/// Halt the CPU forever. QEMU keeps running, so screenshots can still be taken.
pub(crate) fn halt() -> ! {
    x86_64::instructions::interrupts::disable();
    loop {
        x86_64::instructions::hlt();
    }
}
