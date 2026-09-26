//! LazyOS kernel entry point.

#![no_std]
#![no_main]

extern crate alloc;

#[macro_use]
mod macros;

mod console;
mod font;
mod gfx;
mod logging;
mod mem;
mod serial;
mod skia;

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::info::Optional;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;

/// Request a full physical-memory mapping so the kernel can inspect and edit
/// page tables.
const CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    serial::init();
    serial_println!("LazyOS: kernel entered");
    serial_println!("LazyOS: firmware handoff at {:#x}", boot_info.kernel_addr);

    let (base, info) = match &mut boot_info.framebuffer {
        Optional::Some(framebuffer) => {
            let base = framebuffer.buffer_mut().as_mut_ptr() as usize;
            (base, framebuffer.info())
        }
        Optional::None => {
            serial_println!("LazyOS: no framebuffer available; halting");
            halt();
        }
    };

    serial_println!(
        "LazyOS: framebuffer {}x{} stride {} bpp {} format {:?}",
        info.width,
        info.height,
        info.stride,
        info.bytes_per_pixel,
        info.pixel_format
    );

    console::init(base, info);
    boot_banner();

    let stats = mem::init(boot_info);
    println!();
    println!(
        "memory: heap {} KiB, frames {}/{} used",
        stats.heap_size / 1024,
        stats.frames_allocated,
        stats.frames_total
    );
    alloc_demo();

    // Graphics: render a tiny-skia scene and blit it, then label it with text
    // drawn over the image (the console blends over existing pixels).
    skia::render_demo();
    console::reset_cursor();
    println!("tiny-skia");
    println!("anti-aliased 2D on the CPU, blitted to the framebuffer");
    println!("{}x{} {:?}", info.width, info.height, info.pixel_format);

    serial_println!("LazyOS: tiny-skia demo rendered; entering idle loop");
    halt();
}

fn boot_banner() {
    println!("LazyOS");
    println!("single-tasking x86_64 experiment");
    println!();
    println!("The quick brown fox jumps over the lazy dog");
    println!("ABCDEFGHIJKLMNOPQRSTUVWXYZ");
    println!("abcdefghijklmnopqrstuvwxyz");
    println!("0123456789 !\"#$%&'()*+,-./:;<=>?@[\\]^_`{{|}}~");
    println!();
    println!("Anti-aliased JetBrains Mono, rendered from a TTF at build time.");
    println!();
    println!("Scrolling to exercise wrapping and scrollback:");
    for i in 1..=40 {
        println!("  line {0:02}  the quick brown fox 0123456789", i);
    }
}

/// Exercise the heap: allocate, grow, format, and drop.
fn alloc_demo() {
    use alloc::boxed::Box;
    use alloc::format;
    use alloc::string::String;
    use alloc::vec::Vec;

    let mut values: Vec<u32> = Vec::new();
    for i in 0..1000 {
        values.push(i);
    }
    let sum: u32 = values.iter().sum();

    let boxed = Box::new(0xABCD_u32);
    let message: String = format!(
        "alloc: Vec<u32> len {} sum {}; Box {:#06x}",
        values.len(),
        sum,
        *boxed
    );
    println!("{}", message);
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!("LazyOS PANIC: {}", info);
    halt();
}

/// Halt the CPU forever. QEMU keeps running, so screenshots can still be taken.
fn halt() -> ! {
    x86_64::instructions::interrupts::disable();
    loop {
        x86_64::instructions::hlt();
    }
}
