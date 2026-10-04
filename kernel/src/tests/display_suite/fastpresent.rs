//! The fast present (docs/performance-plan.md P3.2): the screen-buffer byte
//! order a compositor can declare (ops 7 and 8), the blit that copies a
//! native row as is and converts every other pairing, and the chunked
//! present that re-validates the grant and the rows after every breath.

use super::present::{bind_screen, paint, present, shows, unbind_screen, Screen, EFAULT, PAGE};
use super::*;
use crate::display::{chunk_rows, layout, op, present_hooks};
use crate::gfx::{Framebuffer, Layout};
use bootloader_api::info::{FrameBufferInfo, PixelFormat};
use core::sync::atomic::{AtomicU64, Ordering};

/// Guard bytes around a fake framebuffer.
const GUARD: usize = 256;
const GUARD_BYTE: u8 = 0x5A;

/// A fake `width x height` framebuffer (stride `width + 3`) in heap memory.
struct Fake {
    memory: Vec<u8>,
    info: FrameBufferInfo,
}

impl Fake {
    fn new(width: usize, height: usize, bpp: usize, format: PixelFormat) -> Fake {
        let stride = width + 3;
        let byte_len = stride * height * bpp;
        let mut memory = vec![GUARD_BYTE; byte_len + 2 * GUARD];
        memory[GUARD..GUARD + byte_len].fill(0);
        let info = FrameBufferInfo {
            byte_len,
            width,
            height,
            pixel_format: format,
            bytes_per_pixel: bpp,
            stride,
        };
        Fake { memory, info }
    }

    fn fb(&mut self) -> Framebuffer {
        Framebuffer::new(self.memory.as_mut_ptr() as usize + GUARD, self.info)
    }

    fn guards_intact(&self) -> bool {
        let end = self.memory.len() - GUARD;
        self.memory[..GUARD].iter().all(|&b| b == GUARD_BYTE)
            && self.memory[end..].iter().all(|&b| b == GUARD_BYTE)
    }
}

/// The colour of source pixel `i` (distinct channels, so a swap shows).
fn color(i: usize) -> (u8, u8, u8) {
    (10 + i as u8, 100 + i as u8, 250 - i as u8)
}

/// A `w x h` source image in `layout`, pixel `i` = [`color`]`(i)`.
fn image(w: usize, h: usize, layout: Layout) -> Vec<u8> {
    (0..w * h)
        .flat_map(|i| {
            let (r, g, b) = color(i);
            match layout {
                Layout::Rgba => [r, g, b, 0x11],
                Layout::Bgra => [b, g, r, 0x11],
            }
        })
        .collect()
}

/// Whether the framebuffer pixel shows `rgb` (grey modes: its luma).
fn matches(fb: &Framebuffer, x: usize, y: usize, rgb: (u8, u8, u8), grey: bool) -> bool {
    let got = fb.read_pixel(x, y);
    if grey {
        let luma = (u32::from(rgb.0) * 77 + u32::from(rgb.1) * 150 + u32::from(rgb.2) * 29) >> 8;
        return u32::from(got.r).abs_diff(luma) <= 1;
    }
    (got.r, got.g, got.b) == rgb
}

/// Every packing against both source layouts: the copied rectangle shows the
/// source colours, a blit clipped by the right edge stops there, nothing
/// outside is written, and the guards hold.
pub fn fast_blit_every_packing_and_layout() -> Result<(), String> {
    const SW: usize = 6;
    const SH: usize = 4;
    let formats = [
        (PixelFormat::Rgb, 4),
        (PixelFormat::Bgr, 4),
        (PixelFormat::Rgb, 3),
        (PixelFormat::Bgr, 3),
        (PixelFormat::U8, 1),
    ];
    for (format, bpp) in formats {
        for layout in [Layout::Rgba, Layout::Bgra] {
            let mut fake = Fake::new(16, 8, bpp, format);
            let mut fb = fake.fb();
            let src = image(SW, SH, layout);
            fb.blit_region(&src, layout, SW, SH, 1, 1, 3, 2, 4, 2);
            // Clipped: from x = 14 only two columns fit.
            fb.blit_region(&src, layout, SW, SH, 0, 0, 14, 6, 4, 1);
            let grey = format == PixelFormat::U8;
            for (row, col) in (0..2).flat_map(|r| (0..4).map(move |c| (r, c))) {
                let want = color((1 + row) * SW + 1 + col);
                check!(
                    matches(&fb, 3 + col, 2 + row, want, grey),
                    "{format:?}/{bpp} {layout:?}: pixel ({}, {}) wrong",
                    3 + col,
                    2 + row
                );
            }
            check!(
                matches(&fb, 14, 6, color(0), grey) && matches(&fb, 15, 6, color(1), grey),
                "{format:?}/{bpp} {layout:?}: clipped blit wrong"
            );
            let untouched = (0..8).all(|y| {
                (0..16).all(|x| {
                    let inside = (2..4).contains(&y) && (3..7).contains(&x);
                    let clipped = y == 6 && x >= 14;
                    inside || clipped || {
                        let p = fb.read_pixel(x, y);
                        (p.r, p.g, p.b) == (0, 0, 0)
                    }
                })
            });
            check!(untouched, "{format:?}/{bpp} {layout:?}: wrote outside");
            check!(fake.guards_intact(), "{format:?}/{bpp} {layout:?}: guards");
        }
    }
    Ok(())
}

/// A source slice shorter than its geometry ends the blit at the last whole
/// row it holds, and offsets past the source or the framebuffer do nothing.
pub fn fast_blit_short_and_outside() -> Result<(), String> {
    let mut fake = Fake::new(8, 8, 4, PixelFormat::Bgr);
    let mut fb = fake.fb();
    let src = image(4, 4, Layout::Bgra);
    // Claims 4x4 but holds two rows and one pixel.
    fb.blit_region(&src[..4 * 4 * 2 + 4], Layout::Bgra, 4, 4, 0, 0, 0, 0, 4, 4);
    check!(matches(&fb, 3, 1, color(7), false), "second row missing");
    let row2 = fb.read_pixel(0, 2);
    check!((row2.r, row2.g, row2.b) == (0, 0, 0), "partial row drawn");
    fb.blit_region(&src, Layout::Bgra, 4, 4, 4, 0, 0, 0, 1, 1);
    fb.blit_region(&src, Layout::Bgra, 4, 4, 0, 0, 8, 0, 1, 1);
    fb.blit_region(&src, Layout::Bgra, 0, 0, 0, 0, 0, 0, 1, 1);
    check!(fake.guards_intact(), "guards");
    Ok(())
}

/// Write one pixel of `rgb` in `layout` into the screen buffer.
fn paint_in(screen: &Screen, x: usize, y: usize, rgb: (u8, u8, u8), layout: Layout) {
    match layout {
        Layout::Rgba => paint(screen, x, y, rgb),
        Layout::Bgra => paint(screen, x, y, (rgb.2, rgb.1, rgb.0)),
    }
}

/// Ops 7 and 8: the native order matches the framebuffer, an unknown order
/// is refused, a declared BGRA buffer presents the right colours, a non-owner
/// is refused, and a new bind starts at RGBA again.
pub fn fast_present_layout_ops() -> Result<(), String> {
    let screen = bind_screen()?;
    let native = process::dispatch_for_test(12, op::NATIVE_LAYOUT, 0, 0);
    let want = crate::console::with_framebuffer(|fb| fb.native_layout()).flatten();
    let expected = match want {
        Some(Layout::Rgba) => layout::RGBA,
        Some(Layout::Bgra) => layout::BGRA,
        None => failed(2),
    };
    check!(
        native == expected,
        "native layout {native:#x}, want {expected:#x}"
    );
    let bad = process::dispatch_for_test(12, op::SET_LAYOUT, 2, 0);
    check!(bad == failed(22), "layout 2 -> {bad:#x}");
    for (index, layout_code) in [layout::BGRA, layout::RGBA, layout::BGRA]
        .into_iter()
        .enumerate()
    {
        let code = process::dispatch_for_test(12, op::SET_LAYOUT, layout_code, 0);
        check!(code == 0, "set layout {layout_code} -> {code:#x}");
        let layout = if layout_code == layout::BGRA {
            Layout::Bgra
        } else {
            Layout::Rgba
        };
        let rgb = (0xE0, 0x40 + index as u8 * 0x20, 0x10);
        paint_in(&screen, 7, 7, rgb, layout);
        check!(present(7, 7, 1, 1) == 0, "present");
        check!(
            shows(7, 7, rgb)?,
            "{layout:?} pixel shown with the wrong colours"
        );
    }
    unbind_screen()?;
    // Its validation guard must end before the next bind (a kernel buffer).
    drop(screen);
    task::harness::switch_current(task::KERNEL_TASK);
    let refused = process::dispatch_for_test(12, op::SET_LAYOUT, layout::BGRA, 0);
    check!(refused == failed(1), "non-owner set layout -> {refused:#x}");
    // A new bind reads RGBA again.
    let screen = bind_screen()?;
    paint(&screen, 2, 2, (0x20, 0xC0, 0x80));
    check!(present(2, 2, 1, 1) == 0, "present after rebind");
    check!(
        shows(2, 2, (0x20, 0xC0, 0x80))?,
        "rebind kept the old layout"
    );
    unbind_screen()
}

/// A full-screen present is copied in chunks with a breath between each
/// two, and every boundary row lands, in both layouts.
pub fn fast_present_full_screen_chunks() -> Result<(), String> {
    let screen = bind_screen()?;
    let step = chunk_rows(screen.width);
    let chunks = screen.height.div_ceil(step);
    check!(chunks > 1, "a full screen should take several chunks");
    // The first and last rows, and both sides of every chunk boundary.
    let mut rows = vec![0, screen.height - 1];
    for boundary in (step..screen.height).step_by(step) {
        rows.extend([boundary - 1, boundary]);
    }
    let _ = present_hooks::take_breaths();
    for (round, layout) in [Layout::Rgba, Layout::Bgra].into_iter().enumerate() {
        let code = if layout == Layout::Bgra {
            layout::BGRA
        } else {
            layout::RGBA
        };
        check!(
            process::dispatch_for_test(12, op::SET_LAYOUT, code, 0) == 0,
            "set layout"
        );
        for (index, &row) in rows.iter().enumerate() {
            let x = (index * 13 + round) % screen.width;
            paint_in(
                &screen,
                x,
                row,
                (0x30 + round as u8, index as u8, 0xC0),
                layout,
            );
        }
        check!(
            present(0, 0, screen.width, screen.height) == 0,
            "full present"
        );
        for (index, &row) in rows.iter().enumerate() {
            let x = (index * 13 + round) % screen.width;
            check!(
                shows(x, row, (0x30 + round as u8, index as u8, 0xC0))?,
                "{layout:?}: row {row} not presented"
            );
        }
    }
    let breaths = present_hooks::take_breaths();
    check!(
        breaths == 2 * (chunks - 1),
        "{breaths} breaths for two presents of {chunks} chunks"
    );
    unbind_screen()
}

/// The page the breath hook unmaps (0: none).
static HOLE: AtomicU64 = AtomicU64::new(0);

fn unmap_hole_at_first_breath(number: usize) {
    let page = HOLE.load(Ordering::Relaxed);
    if number == 1 && page != 0 {
        crate::mem::unmap_range(crate::mem::kernel_table(), page, page + PAGE);
    }
}

fn unbind_at_first_breath(number: usize) {
    if number == 1 {
        process::dispatch_for_test(12, op::UNBIND, 0, 0);
    }
}

/// What another thread does while a present breathes is seen by the next
/// chunk: a page of the last chunk unmapped then is `-EFAULT` with the first
/// chunk already shown, and a grant gone then ends the present quietly.
pub fn fast_present_chunks_revalidate() -> Result<(), String> {
    let screen = bind_screen()?;
    let last_row = screen.height - 1;
    let page = (screen.va + last_row as u64 * screen.row_bytes()) & !(PAGE - 1);
    HOLE.store(page, Ordering::Relaxed);
    paint(&screen, 4, 0, (0xF0, 0x80, 0x10));
    present_hooks::set_hook(Some(unmap_hole_at_first_breath));
    let code = present(0, 0, screen.width, screen.height);
    present_hooks::set_hook(None);
    HOLE.store(0, Ordering::Relaxed);
    check!(code == failed(EFAULT), "unmapped mid-present -> {code:#x}");
    check!(shows(4, 0, (0xF0, 0x80, 0x10))?, "first chunk not shown");
    unbind_screen()?;
    drop(screen);

    let screen = bind_screen()?;
    let row = chunk_rows(screen.width) + 1;
    paint(&screen, 6, row, (0x12, 0x34, 0x56));
    present_hooks::set_hook(Some(unbind_at_first_breath));
    let code = present(0, 0, screen.width, screen.height);
    present_hooks::set_hook(None);
    check!(code == 0, "grant gone mid-present -> {code:#x}");
    check!(
        !shows(6, row, (0x12, 0x34, 0x56))?,
        "a chunk was copied from a grant that was gone"
    );
    let _ = present_hooks::take_breaths();
    // The hook unbound already; only the task state is left to restore.
    task::harness::switch_current(task::KERNEL_TASK);
    crate::display::reset();
    task::harness::reset();
    Ok(())
}

/// Hundreds of full-screen presents, alternating layouts and content: every
/// one succeeds, takes exactly its breaths, and shows its last pixel.
pub fn fast_present_full_screen_soak() -> Result<(), String> {
    const ROUNDS: usize = 300;
    let screen = bind_screen()?;
    let chunks = screen.height.div_ceil(chunk_rows(screen.width));
    let _ = present_hooks::take_breaths();
    for round in 0..ROUNDS {
        let (code, layout) = if round % 2 == 0 {
            (layout::RGBA, Layout::Rgba)
        } else {
            (layout::BGRA, Layout::Bgra)
        };
        check!(
            process::dispatch_for_test(12, op::SET_LAYOUT, code, 0) == 0,
            "set layout"
        );
        let (x, y) = ((round * 31) % screen.width, (round * 17) % screen.height);
        let rgb = (round as u8, 0x80, 0xFF - round as u8);
        paint_in(&screen, x, y, rgb, layout);
        check!(
            present(0, 0, screen.width, screen.height) == 0,
            "round {round}"
        );
        check!(shows(x, y, rgb)?, "round {round}: ({x}, {y}) not shown");
    }
    let breaths = present_hooks::take_breaths();
    check!(breaths == ROUNDS * (chunks - 1), "{breaths} breaths");
    unbind_screen()
}
