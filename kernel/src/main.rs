//! LazyOS kernel entry point.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

#[macro_use]
mod macros;

mod arch;
mod block;
mod console;
mod cursor;
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
mod serial;
#[allow(dead_code)]
mod skia;
mod surface;
mod task;
#[cfg(laZYOS_TESTS)]
mod tests;
mod text;

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
#[cfg_attr(laZYOS_TESTS, allow(unreachable_code))]
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

    mem::init(boot_info);

    // Kernel test mode (issue #62): run the in-kernel suite and halt instead of
    // booting the demo. Compiled in only with `LAZYOS_TESTS=1`.
    #[cfg(laZYOS_TESTS)]
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
        spawn_program("hello", "HELLO.ELF");
        spawn_program("sh", "SH.ELF");
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
