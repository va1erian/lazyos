//! LazyOS kernel entry point.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

#[macro_use]
mod macros;

mod arch;
mod block;
mod cli;
mod console;
mod cursor;
mod font;
mod fs;
mod gfx;
mod gfxlib;
mod input;
mod logging;
mod mem;
mod process;
mod serial;
mod skia;
mod surface;
mod text;

use bootloader_api::config::{BootloaderConfig, Mapping};
use bootloader_api::info::Optional;
use bootloader_api::{entry_point, BootInfo};
use core::panic::PanicInfo;
use gfx::Color;
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

    if fs::init() {
        serial_println!("LazyOS: FAT16 filesystem mounted");
    } else {
        serial_println!("LazyOS: no filesystem found");
    }

    // Draw the tiny-skia background scene (retained for dirty-rect redraws).
    skia::render_demo();

    // Interrupts: PIC + PIT + PS/2 keyboard.
    arch::init();
    x86_64::instructions::interrupts::enable();

    cli_demo();
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

/// Window-hosted CLI with runnable demos.
///
/// Typing edits the prompt; Enter runs a command. Arrow keys move the window,
/// Page Up/Down scroll the output. `box` and `ball` render demos in the body.
/// Drawing is double buffered to avoid flicker.
fn cli_demo() -> ! {
    let (fbw, fbh) =
        console::with_framebuffer(|fb| (fb.width(), fb.height())).unwrap_or((1280, 720));

    let mut back = surface::RgbaBuffer::new(fbw, fbh);
    skia::background_blit_into(&mut back, 0, 0, fbw, fbh);

    let mut cli = match cli::Cli::new(120, 70, 660, 460) {
        Some(cli) => cli,
        None => halt(),
    };
    redraw_rect(&mut back, &cli, cli.rect(), fbw, fbh);
    serial_println!("LazyOS: CLI ready (type 'help')");

    input::mouse::set_bounds(fbw as i32, fbh as i32);
    let mut cursor_pos: Option<(i32, i32)> = None;

    loop {
        let mut repaint = false;

        if let Some(key) = input::keyboard::try_read_key() {
            let before = cli.rect();
            match key {
                Key::Left | Key::Right | Key::Up | Key::Down => {
                    let (dx, dy) = match key {
                        Key::Left => (-16, 0),
                        Key::Right => (16, 0),
                        Key::Up => (0, -16),
                        _ => (0, 16),
                    };
                    cli.move_by(dx, dy, fbw as i32, fbh as i32);
                    redraw_rect(
                        &mut back,
                        &cli,
                        union_rect(before, cli.rect(), fbw, fbh),
                        fbw,
                        fbh,
                    );
                    repaint = true;
                }
                other => match cli.on_key(other) {
                    cli::Effect::None => {}
                    cli::Effect::Redraw => {
                        redraw_rect(&mut back, &cli, cli.rect(), fbw, fbh);
                        repaint = true;
                    }
                    cli::Effect::Ball => {
                        run_ball_demo(&mut back, &cli);
                        redraw_rect(&mut back, &cli, cli.rect(), fbw, fbh);
                        repaint = true;
                    }
                },
            }
        }

        // Cursor overlay: erase the old sprite from the back buffer, then draw
        // the new one. Window repaints may overwrite it, so redraw when needed.
        if let Some((nx, ny)) = input::mouse::take_moved() {
            if let Some((px, py)) = cursor_pos {
                present(
                    &back,
                    px.max(0) as usize,
                    py.max(0) as usize,
                    cursor::WIDTH as usize,
                    cursor::HEIGHT as usize,
                );
            }
            cursor_pos = Some((nx, ny));
            draw_cursor((nx, ny));
        } else if repaint {
            if let Some(pos) = cursor_pos {
                draw_cursor(pos);
            }
        }

        if cursor_pos.is_none() {
            let state = input::mouse::state();
            cursor_pos = Some((state.x, state.y));
            draw_cursor((state.x, state.y));
        }

        x86_64::instructions::hlt();
    }
}

/// Draw the cursor sprite directly onto the live framebuffer.
fn draw_cursor(pos: (i32, i32)) {
    console::with_framebuffer(|fb| cursor::draw(fb, pos.0, pos.1));
}

/// Compose a screen rectangle into the back buffer and present it in one blit.
fn redraw_rect(
    back: &mut surface::RgbaBuffer,
    cli: &cli::Cli,
    rect: (i32, i32, i32, i32),
    _fbw: usize,
    _fbh: usize,
) {
    let (x, y, w, h) = rect;
    if w <= 0 || h <= 0 {
        return;
    }
    let (x, y, w, h) = (x.max(0) as usize, y.max(0) as usize, w as usize, h as usize);
    skia::background_blit_into(back, x, y, w, h);
    cli.draw_into(back);
    present(back, x, y, w, h);
}

/// Present a rectangle of the back buffer to the live framebuffer.
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
) -> (i32, i32, i32, i32) {
    let x0 = a.0.min(b.0).max(0);
    let y0 = a.1.min(b.1).max(0);
    let x1 = (a.0 + a.2).max(b.0 + b.2).min(fw as i32);
    let y1 = (a.1 + a.3).max(b.1 + b.3).min(fh as i32);
    (x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
}

/// Demo 2: bouncing squares animated at ~30 fps until a key is pressed.
fn run_ball_demo(back: &mut surface::RgbaBuffer, cli: &cli::Cli) {
    let (cx, cy, cw, ch) = cli.content_rect();
    let size = 20.0f32;
    let base = [
        (100.0f32, 60.0f32, 4.0f32, 3.0f32, Color::rgb(239, 71, 111)),
        (200.0, 140.0, -3.0, 4.0, Color::rgb(255, 209, 102)),
        (320.0, 90.0, 5.0, -3.0, Color::rgb(6, 214, 160)),
        (420.0, 180.0, -4.0, -4.0, Color::rgb(17, 138, 178)),
        (260.0, 220.0, 3.0, 5.0, Color::rgb(150, 120, 255)),
    ];
    // Offset ball starts relative to the content area.
    let mut balls = base;
    for ball in balls.iter_mut() {
        ball.0 += cx as f32;
        ball.1 += cy as f32;
    }

    let left = cx as f32;
    let right = (cx + cw) as f32 - size;
    let top = cy as f32;
    let bottom = (cy + ch) as f32 - size;

    let mut last = arch::ticks();
    loop {
        if input::keyboard::try_read_key().is_some() {
            break;
        }
        let now = arch::ticks();
        if now.wrapping_sub(last) < 3 {
            x86_64::instructions::hlt();
            continue;
        }
        last = now;

        for ball in balls.iter_mut() {
            ball.0 += ball.2;
            ball.1 += ball.3;
            if ball.0 < left {
                ball.0 = left;
                ball.2 = ball.2.abs();
            }
            if ball.0 > right {
                ball.0 = right;
                ball.2 = -ball.2.abs();
            }
            if ball.1 < top {
                ball.1 = top;
                ball.3 = ball.3.abs();
            }
            if ball.1 > bottom {
                ball.1 = bottom;
                ball.3 = -ball.3.abs();
            }
        }

        cli.clear_content(back);
        for (x, y, _, _, color) in balls.iter() {
            back.fill_rect(
                *x as i32,
                *y as i32,
                (*x + size) as i32,
                (*y + size) as i32,
                *color,
            );
        }
        present(back, cx as usize, cy as usize, cw as usize, ch as usize);
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
