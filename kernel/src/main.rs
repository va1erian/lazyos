//! LazyOS kernel entry point.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

#[macro_use]
mod macros;

mod arch;
mod console;
mod font;
mod gfx;
mod input;
mod logging;
mod mem;
mod serial;
mod skia;
mod text;
mod window;

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::info::Optional;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use input::keyboard::Key;

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

    // Draw the tiny-skia background scene (retained for dirty-rect redraws).
    skia::render_demo();

    // Interrupts: PIC + PIT + PS/2 keyboard.
    arch::init();
    x86_64::instructions::interrupts::enable();

    window_demo();
}

fn boot_banner() {
    println!("LazyOS");
    println!("single-tasking x86_64 experiment");
    println!();
    println!("The quick brown fox jumps over the lazy dog");
    println!("ABCDEFGHIJKLMNOPQRSTUVWXYZ");
    println!("abcdefghijklmnopqrstuvwxyz");
    println!("0123456789 !\"#$%&'()*+,-./:;<=>?@[\\]^_`{{|}}~");
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

/// Interactive demo: a movable window with scrollable text.
///
/// Arrow keys move the window; Page Up / Page Down scroll a page; Home / End
/// jump to the top / bottom. Only the window's footprint is repainted.
fn window_demo() -> ! {
    let (fbw, fbh) = console::with_framebuffer(|fb| (fb.width() as i32, fb.height() as i32))
        .unwrap_or((1280, 720));

    let mut win = match window::Window::new(140, 90, 560, 400) {
        Some(win) => win,
        None => halt(),
    };
    win.render();
    serial_println!("LazyOS: window demo ready (arrows move, PgUp/PgDn scroll)");

    let step = 16;
    loop {
        let key = input::keyboard::read_key();
        let old = (win.x, win.y, win.scroll);
        let mut moved = false;

        match key {
            Key::Left => {
                win.move_to(win.x - step, win.y, fbw, fbh);
                moved = true;
            }
            Key::Right => {
                win.move_to(win.x + step, win.y, fbw, fbh);
                moved = true;
            }
            Key::Up => {
                win.move_to(win.x, win.y - step, fbw, fbh);
                moved = true;
            }
            Key::Down => {
                win.move_to(win.x, win.y + step, fbw, fbh);
                moved = true;
            }
            Key::PageUp => {
                win.scroll_page(-1);
            }
            Key::PageDown => {
                win.scroll_page(1);
            }
            Key::Home => {
                win.scroll_to(0);
            }
            Key::End => {
                win.scroll_to(usize::MAX);
            }
            _ => {}
        }

        if moved || win.scroll != old.2 {
            // Repaint only the affected footprint: restore the old and new
            // rectangles from the retained background, then redraw the window.
            restore_footprint(old.0, old.1, win.w, win.h);
            restore_footprint(win.x, win.y, win.w, win.h);
            win.render();
        }
    }
}

/// Restore a screen rectangle from the retained tiny-skia background.
fn restore_footprint(x: i32, y: i32, w: i32, h: i32) {
    if x < 0 || y < 0 {
        return;
    }
    console::with_framebuffer(|fb| {
        let cw = (w as usize).min(fb.width().saturating_sub(x as usize));
        let ch = (h as usize).min(fb.height().saturating_sub(y as usize));
        if cw > 0 && ch > 0 {
            skia::restore_region(fb, x as usize, y as usize, cw, ch);
        }
    });
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
