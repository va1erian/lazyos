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

mod arch;
mod block;
mod console;
mod cursor;
mod display;
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

    mem::init(boot_info);

    // Kernel test mode (issue #62): run the in-kernel suite and halt instead of
    // booting the demo. Compiled in only with `LAZYOS_TESTS=1`.
    #[cfg(lazyos_tests)]
    tests::run();

    if fs::init() {
        serial_println!("LazyOS: FAT16 filesystem mounted");
    } else {
        serial_println!("LazyOS: no filesystem found");
    }

    // Descriptor tables, interrupts (PIC/PIT), and the PS/2 mouse.
    arch::init();
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
    if let Some(bytes) = fs::read("BUSYBOX") {
        serial_println!("LazyOS: launching busybox sh");
        match task::spawn_linux("sh", &bytes, "sh") {
            Ok(index) => serial_println!("LazyOS: spawned busybox as task {index}"),
            Err(err) => serial_println!("LazyOS: spawn busybox failed: {err}"),
        }
    } else if let Some(bytes) = fs::read("INIT.ELF") {
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
        spawn_program("init", "SUPER.ELF");

        // With `LAZYOS_MESSENGERCTL=1` too, the fabric tool also boots, so a
        // scripted session can query the new `services`/`health`/`log`
        // commands against the running supervisor (issue #93).
        #[cfg(all(services_mode, messengerctl_demo))]
        spawn_program("messengerctl", "MSGCTL.ELF");

        // Issue #89: `LAZYOS_MESSENGERD=1` starts the registry daemon before
        // the demo programs. It claims the bootstrap channel and serves name
        // requests for the life of the system. The on-disk name is 8.3-safe
        // (`MSGRD.ELF`: the kernel's FAT reader has no long-name support).
        #[cfg(all(messengerd_service, not(services_mode)))]
        spawn_program("messengerd", "MSGRD.ELF");

        // `LAZYOS_MESSENGERCTL=1` swaps the hello window for the fabric
        // snapshot tool (issue #70); the default demo is unchanged. The file
        // name is 8.3: the kernel FAT reader has no long-name support.
        #[cfg(all(messengerctl_demo, not(services_mode)))]
        spawn_program("messengerctl", "MSGCTL.ELF");
        #[cfg(all(not(messengerctl_demo), not(services_mode)))]
        spawn_program("hello", "HELLO.ELF");
        #[cfg(not(services_mode))]
        spawn_program("sh", "SH.ELF");

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
        #[cfg(all(xuid_demo, not(xui_app)))]
        spawn_program("xuid", "XUID.ELF");
        #[cfg(all(xuid_demo, not(xui_app)))]
        spawn_program("xdemo", "XDEMO.ELF");
        #[cfg(all(xuid_demo, not(xui_app)))]
        spawn_program("xdemo", "XDEMO.ELF");
        #[cfg(all(xui_app, not(xui_client)))]
        spawn_linux_program("xapp", "XAPP.ELF");
        #[cfg(xui_client)]
        spawn_program("xuid", "XUID.ELF");
        #[cfg(xui_client)]
        spawn_linux_program("xapp", "XAPP.ELF");

        // Issue #145: the drag & drop demo pair. `dragdemo` is a launcher that
        // starts a drag source and a drop target child, so a scripted session
        // can drag a typed payload from one surface to the other (or cancel it
        // with Escape) and grep the `DND:*` evidence markers from serial. It
        // also starts `clipboardd` (`CLIPD.ELF`) when the supervisor is not
        // running, since the token transfer needs the clipboard service. The
        // xui app owns the display grant, so the demo skips that image; with
        // 64 task slots (issue #204) it fits next to the services too.
        #[cfg(all(xuid_demo, not(xui_app)))]
        spawn_program("dragdemo", "DRAGDMO.ELF");

        // Issue #167: the shell-protocol evidence client. The
        // `LAZYOS_SHELLPROBE=1` demo hook keeps the default `xuid` sessions
        // untouched; when set it boots `shellprobe`, which creates the desktop
        // surface, subscribes to the shell events, and logs the
        // `SHELLPROBE:*:PASS` markers.
        #[cfg(all(xuid_demo, shellprobe_demo, not(xui_app)))]
        spawn_program("shellprobe", "SHELLPRB.ELF");
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

    task::start();
    serial_println!("LazyOS: scheduler started (Tab switches focus)");
    x86_64::instructions::interrupts::enable();

    // The kernel task becomes the terminal multiplexer.
    mux::run();
}

/// Load a program from the FAT disk and spawn it as a task, if present.
fn spawn_program(name: &'static str, path: &str) {
    match fs::read(path) {
        Some(bytes) => match task::spawn(name, &bytes) {
            Ok(index) => serial_println!("LazyOS: spawned {name} as task {index}"),
            Err(err) => serial_println!("LazyOS: spawn {name} failed: {err}"),
        },
        None => serial_println!("LazyOS: {path} not found"),
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
