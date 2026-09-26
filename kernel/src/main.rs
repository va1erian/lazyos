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
mod surface;
mod text;
mod window;

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::info::Optional;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use input::keyboard::Key;
use surface::Surface;

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
/// jump to the top / bottom. Drawing happens off-screen and is presented in a
/// single blit (double buffering) to avoid flicker.
fn window_demo() -> ! {
    let (fbw, fbh) =
        console::with_framebuffer(|fb| (fb.width(), fb.height())).unwrap_or((1280, 720));

    // Back buffer seeded with the static scene.
    let mut back = surface::RgbaBuffer::new(fbw, fbh);
    skia::background_blit_into(&mut back, 0, 0, fbw, fbh);

    let mut win = match window::Window::new(140, 90, 560, 400) {
        Some(win) => win,
        None => halt(),
    };
    win.render_into(&mut back);
    present(&back, 0, 0, fbw, fbh);
    serial_println!("LazyOS: window demo ready (arrows move, PgUp/PgDn scroll)");

    let step = 16;
    loop {
        let key = input::keyboard::read_key();
        let before = win.rect();
        let scroll_before = win.scroll;

        match key {
            Key::Left => win.move_to(win.x - step, win.y, fbw as i32, fbh as i32),
            Key::Right => win.move_to(win.x + step, win.y, fbw as i32, fbh as i32),
            Key::Up => win.move_to(win.x, win.y - step, fbw as i32, fbh as i32),
            Key::Down => win.move_to(win.x, win.y + step, fbw as i32, fbh as i32),
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

        let after = win.rect();
        if after != before || win.scroll != scroll_before {
            // Compose the dirty union off-screen, then present it in one blit.
            let (x, y, w, h) = union_rect(before, after, fbw, fbh);
            skia::background_blit_into(&mut back, x, y, w, h);
            win.render_into(&mut back);
            present(&back, x, y, w, h);
        }
    }
}

/// Present a rectangle of the back buffer to the live framebuffer in one pass.
fn present(back: &surface::RgbaBuffer, x: usize, y: usize, w: usize, h: usize) {
    console::with_framebuffer(|fb| {
        fb.blit_rgba_region(back.data(), back.width(), back.height(), x, y, x, y, w, h);
    });
}

/// Union of two screen rectangles, clamped to the screen.
fn union_rect(
    a: (i32, i32, i32, i32),
    b: (i32, i32, i32, i32),
    fw: usize,
    fh: usize,
) -> (usize, usize, usize, usize) {
    let x0 = a.0.min(b.0).max(0);
    let y0 = a.1.min(b.1).max(0);
    let x1 = (a.0 + a.2).max(b.0 + b.2).min(fw as i32);
    let y1 = (a.1 + a.3).max(b.1 + b.3).min(fh as i32);
    (
        x0 as usize,
        y0 as usize,
        (x1 - x0).max(0) as usize,
        (y1 - y0).max(0) as usize,
    )
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
