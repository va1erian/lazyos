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
mod mem;
mod mux;
mod process;
mod serial;
#[allow(dead_code)]
mod skia;
mod surface;
mod task;
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
