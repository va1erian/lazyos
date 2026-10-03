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
mod boot_media;
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
mod klog;
mod limits;
mod mem;
mod mux;
mod panic_screen;
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
mod tty;
mod user_ptr;
mod wallclock;

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::info::Optional;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;

/// Request a full physical-memory mapping so the kernel can manage page tables,
/// and keep every bootloader mapping (kernel image, boot stack, boot info,
/// framebuffer, physical map) in the kernel half below the heap, so the whole
/// lower half below the shared-buffer window is user address space
/// (`mem::layout`).
const CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config.mappings.dynamic_range_start = Some(mem::BOOT_DYNAMIC_START);
    config.mappings.dynamic_range_end = Some(mem::BOOT_DYNAMIC_END);
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

    // Firmware geometry is input: clamp it before anything sizes from it.
    let info = gfx::sanitize(info);
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
    // `mem::init` keeps the boot info borrowed, so read the ramdisk hand-off
    // and the firmware type (`BOOT:MEDIA:<uefi|bios>`) first.
    let (ramdisk_addr, ramdisk_len) = (boot_info.ramdisk_addr, boot_info.ramdisk_len);
    // The ACPI tables (read by `arch::init`'s tick selection) start at the RSDP.
    arch::acpi_tables::set_rsdp(boot_info.rsdp_addr.into_option());
    boot_media::record(&boot_info.memory_regions);
    mem::init(boot_info);
    // Machine-derived limits, before anything sizes itself from one;
    // `lazyos.cfg` can override them once the boot volume is mounted.
    limits::init_for_machine(mem::usable_ram(), display::screen_bytes());
    boot_phase!("mem_ready");
    // Firmware usually leaves the framebuffer uncached: make it write-combining
    // (bare metal only, see `mem::wc::under_hypervisor`).
    if mem::wc::under_hypervisor() {
        serial_println!("HW:FB:WC:SKIPPED (hypervisor: the framebuffer is guest RAM)");
    } else if let Some((fb_base, fb_len)) = console::framebuffer_span() {
        match mem::wc::map_write_combining(fb_base, fb_len) {
            Ok(pages) => serial_println!("HW:FB:WC:{pages} pages write-combining"),
            Err(reason) => serial_println!("HW:FB:WC:SKIPPED ({reason})"),
        }
    }

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

    // Issue #5: a bootloader ramdisk is registered as `ram0` and its MBR
    // partitions as `ram0p<n>`; when it is there, `fs::init` looks for the
    // boot volume and the root on it first (the USB stick's RAM root,
    // docs/usb-stick.md), so a disk carrying the same volume cannot win.
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
    limits::describe();
    // Descriptor tables, interrupts (PIC/PIT), and the PS/2 mouse.
    arch::init();
    boot_phase!("arch_ready");
    // Interrupt vectors and the device syscall are live: print their evidence.
    dev::selfcheck();
    let screen = display::logical();
    input::mouse::set_bounds(screen.width as i32, screen.height as i32);

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
        match open_program(fhs::bin::BUSYBOX) {
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
            None => serial_println!("ABI:busybox:FAIL:no {} on image", fhs::bin::BUSYBOX),
        }
    } else if let Some(bytes) = open_program(fhs::bin::ABI_INIT) {
        // The ABI bench's injected fixture owns the boot. Checked before the
        // demo profile so a stray BusyBox on an image cannot shadow a fixture.
        serial_println!("ABI:INIT:START");
        match task::spawn_linux("init", &bytes, "init") {
            Ok(index) => serial_println!("LazyOS: spawned init as task {index}"),
            Err(err) => serial_println!("ABI:INIT:FAIL:{err}"),
        }
    } else {
        // Issue #93: `LAZYOS_SERVICES=1` boots the userspace supervisor
        // (`/system/bin/init`). `init` starts the platform services from its manifest
        // (messengerd, logd, healthd, the crash-test service), so the kernel
        // does not spawn them itself; `init` owns supervision, restart policy
        // and the service event log from here on. The ABI bench's fixture
        // lives at `/system/bin/abi-init`, so the hook above never shadows it.
        #[cfg(services_mode)]
        spawn_program(fhs::bin::INIT, &[]);

        // With `LAZYOS_MESSENGERCTL=1` too, the fabric tool also boots, so a
        // scripted session can query the new `services`/`health`/`log`
        // commands against the running supervisor (issue #93).
        #[cfg(all(services_mode, messengerctl_demo))]
        spawn_program(fhs::bin::MESSENGERCTL, &[]);

        // Issue #89: `LAZYOS_MESSENGERD=1` starts the registry daemon before
        // the demo programs. It claims the bootstrap channel and serves name
        // requests for the life of the system.
        #[cfg(all(messengerd_service, not(services_mode)))]
        spawn_program(fhs::bin::MESSENGERD, &[]);

        // `LAZYOS_MESSENGERCTL=1` swaps the hello window for the fabric
        // snapshot tool (issue #70); the default demo is unchanged.
        #[cfg(all(messengerctl_demo, not(services_mode)))]
        spawn_program(fhs::bin::MESSENGERCTL, &[]);
        #[cfg(all(not(messengerctl_demo), not(services_mode), not(cli_mode)))]
        spawn_program(fhs::bin::HELLO, &[]);
        #[cfg(not(services_mode))]
        spawn_console_shell();

        // `LAZYOS_SOUND=1` boots the virtio-sound driver and the system mixer
        // directly when there is no supervisor to start them (docs/driver-plan.md
        // D6, docs/audio-plan.md). `sndd` plays a test tone with `demo=1`, then
        // `audiod demo=1` runs the evidence clients through the mixer; the
        // sound harness records both.
        #[cfg(all(sound_demo, not(services_mode)))]
        spawn_program(fhs::bin::SNDD, &["demo=1"]);
        #[cfg(all(sound_demo, not(services_mode)))]
        spawn_program(fhs::bin::AUDIOD, &["demo=1"]);

        // `LAZYOS_NET=1` boots the virtio-net driver directly when there is no
        // supervisor to start it (docs/networking-plan.md N1). `netdrv` runs
        // its ARP self-test and the `nicctl` evidence clients with `demo=1`.
        #[cfg(all(net_demo, not(services_mode)))]
        spawn_program(fhs::bin::NETDRV, &net_demo_args());
        // `LAZYOS_NETD=1` adds the stack service on top of the driver
        // (docs/networking-plan.md N2); it finds the driver by name and retries,
        // so the order does not matter.
        #[cfg(all(netd_demo, not(services_mode)))]
        spawn_program(fhs::bin::NETD, &netd_demo_args());

        // Issue #113: `LAZYOS_XUID=1` boots the userspace compositor (`xuid`)
        // and two instances of the display-protocol demo app (`xdemo`).
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
        spawn_program(fhs::bin::XUID, &[]);
        #[cfg(all(xuid_demo, not(xui_app), not(xui_desktop)))]
        spawn_program(fhs::bin::XDEMO, &[]);
        #[cfg(all(xuid_demo, not(xui_app), not(xui_desktop)))]
        spawn_program(fhs::bin::XDEMO, &[]);
        #[cfg(all(xuid_demo, xui_desktop, not(xui_app)))]
        spawn_program(fhs::bin::XUID, &[]);
        #[cfg(all(xui_app, not(xui_client)))]
        spawn_linux_program("xapp", fhs::bin::XAPP);
        #[cfg(xui_client)]
        spawn_program(fhs::bin::XUID, &[]);
        #[cfg(xui_client)]
        spawn_linux_program("xapp", fhs::bin::XAPP);

        // Issue #145: the drag & drop demo pair. `dragdemo` is a launcher that
        // starts a drag source and a drop target child, so a scripted session
        // can drag a typed payload from one surface to the other (or cancel it
        // with Escape) and grep the `DND:*` evidence markers from serial. It
        // also starts `clipboardd` when the supervisor is not
        // running, since the token transfer needs the clipboard service. The
        // xui app owns the display grant, so the demo skips that image; with
        // 64 task slots (issue #204) it fits next to the services too.
        #[cfg(all(xuid_demo, not(xui_app), not(xui_desktop)))]
        spawn_program(fhs::bin::DRAGDEMO, &[]);

        // Issue #167: the shell-protocol evidence client. The
        // `LAZYOS_SHELLPROBE=1` demo hook keeps the default `xuid` sessions
        // untouched; when set it boots `shellprobe`, which creates the desktop
        // surface, subscribes to the shell events, and logs the
        // `SHELLPROBE:*:PASS` markers.
        #[cfg(all(xuid_demo, shellprobe_demo, not(xui_app), not(xui_desktop)))]
        spawn_program(fhs::bin::SHELLPROBE, &[]);
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
    // `LAZYOS_FORCE_PANIC=1` at build time: prove the on-screen panic report
    // (docs/real-pc-boot-plan.md H1) with a full boot log behind it.
    if option_env!("LAZYOS_FORCE_PANIC").is_some() {
        panic!("forced by LAZYOS_FORCE_PANIC (on-screen panic test)");
    }
    task::start();
    serial_println!("LazyOS: scheduler started (Tab switches focus)");
    x86_64::instructions::interrupts::enable();

    // The kernel task becomes the terminal multiplexer.
    mux::run();
}

/// Open a program on the OS volume for the loader to stream, as the kernel
/// (root). `None` when it is missing.
fn open_program(path: &str) -> Option<process::image::VfsFile> {
    process::image::VfsFile::native(fs::vfs::Id::current(), path).ok()
}

/// Load a program from the OS volume and spawn it as a task named after its
/// file (`/system/bin/init` runs as `init`), if present. Its `argv` is the
/// path then `args`, recorded in the per-task block `spawnv` fills, item for
/// item (no command line is composed or parsed).
fn spawn_program(path: &'static str, args: &[&str]) {
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
fn net_demo_args() -> alloc::vec::Vec<&'static str> {
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
fn netd_demo_args() -> alloc::vec::Vec<&'static str> {
    const NETD_ARGS: &str = match option_env!("LAZYOS_NETD_ARGS") {
        Some(args) => args,
        None => "demo=1",
    };
    NETD_ARGS.split_ascii_whitespace().collect()
}

/// Load a static Linux-ABI (musl) program and spawn it, if present. The xui
/// app is a `std` binary, so it boots through the Linux path (`spawn_linux`).
#[cfg(xui_app)]
fn spawn_linux_program(name: &'static str, path: &str) {
    match open_program(path) {
        Some(bytes) => match task::spawn_linux(name, &bytes, name) {
            Ok(index) => serial_println!("LazyOS: spawned {name} as task {index}"),
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
fn spawn_console_shell() {
    match open_program(fhs::bin::BUSYBOX) {
        Some(bytes) => {
            serial_println!("LazyOS: launching busybox sh");
            match task::spawn_linux_args("sh", &bytes, &["sh"]) {
                Ok(index) => serial_println!("LazyOS: spawned busybox as task {index}"),
                Err(err) => serial_println!("LazyOS: spawn busybox failed: {err}"),
            }
        }
        None => serial_println!(
            "LazyOS: no {} on the image; no console shell (build it with tools/abi/build.py)",
            fhs::bin::BUSYBOX
        ),
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("LazyOS PANIC: {}", info);
    // A real PC has no serial port: put the reason and the boot log on screen.
    panic_screen::show("LazyOS stopped: kernel panic", format_args!("{}", info));
    halt();
}

/// Halt the CPU forever. QEMU keeps running, so screenshots can still be taken.
pub(crate) fn halt() -> ! {
    x86_64::instructions::interrupts::disable();
    loop {
        x86_64::instructions::hlt();
    }
}
