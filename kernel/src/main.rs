//! LazyOS kernel entry point.

#![no_std]
#![no_main]

#[macro_use]
mod macros;

mod console;
mod font;
mod gfx;
mod serial;

use bootloader_api::info::Optional;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;

entry_point!(kernel_main);

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
    serial_println!("LazyOS: banner drawn; entering idle loop");
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
