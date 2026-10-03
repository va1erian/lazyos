//! The on-screen stop report (H1 of `docs/real-pc-boot-plan.md`).
//!
//! A real PC usually has no serial port, so a panic or `kstop` that only
//! reached COM1 would look like a frozen screen. [`show`] paints the reason
//! and the tail of the boot-log ring (`klog`) straight onto the framebuffer,
//! large enough to read and photograph on a 4K panel.
//!
//! It runs in the worst possible state, so it takes no lock it could wait on
//! (`console::panic_framebuffer` and `klog::snapshot_nowait`), allocates
//! nothing, and draws at most once: a panic inside the drawing just halts.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::font;
use crate::gfx::{Color, Framebuffer};
use crate::surface::Surface;
use crate::text::{self, Rect};

const BACKGROUND: Color = Color::rgb(0x50, 0x0A, 0x0A);
const TITLE: Color = Color::rgb(0xFF, 0xFF, 0xFF);
const TEXT: Color = Color::rgb(0xF0, 0xE0, 0xE0);
const LOG: Color = Color::rgb(0xC8, 0xC0, 0xC0);
/// Text inset from the screen edge, in (unscaled) pixels.
const MARGIN: usize = 16;
/// Bytes of the boot log the report can show.
const TAIL_BYTES: usize = 6 * 1024;
/// Bytes of the formatted reason kept (the rest is cut).
const MESSAGE_BYTES: usize = 768;

static DRAWN: AtomicBool = AtomicBool::new(false);
/// The boot-log tail, outside any stack frame (a panic may be deep in one).
static TAIL: spin::Mutex<[u8; TAIL_BYTES]> = spin::Mutex::new([0; TAIL_BYTES]);

/// Paint `title`, `reason` and the boot log's tail over the whole screen.
/// Safe to call from the panic handler; a second call does nothing.
pub fn show(title: &str, reason: fmt::Arguments) {
    x86_64::instructions::interrupts::disable();
    if DRAWN.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(mut fb) = crate::console::panic_framebuffer() else {
        return;
    };
    let mut message = FixedText::<MESSAGE_BYTES>::new();
    let _ = message.write_fmt(reason);
    let Some(mut tail) = TAIL.try_lock() else {
        return;
    };
    let count = crate::klog::snapshot_nowait(&mut tail[..]);
    render(&mut fb, title, message.as_str(), &tail[..count]);
}

/// Draw the report into `fb` (the kernel suite renders into a fake one).
/// Every pixel goes through the framebuffer's clipped writers.
pub fn render(fb: &mut Framebuffer, title: &str, message: &str, log: &[u8]) {
    let (width, height) = (fb.width(), fb.height());
    fb.fill_rect(0, 0, width, height, BACKGROUND);
    let mut screen = Scaled {
        fb,
        scale: scale_for(width),
    };
    let columns = screen.columns();
    let rows = screen.rows();
    let mut row = 0;
    let line = |screen: &mut Scaled, text: &str, color: Color, row: &mut usize| {
        if *row < rows {
            screen.draw_line(*row, text, color);
            *row += 1;
        }
    };
    line(&mut screen, title, TITLE, &mut row);
    row += 1;
    for chunk in wrap(message, columns) {
        line(&mut screen, chunk, TEXT, &mut row);
    }
    row += 1;
    line(&mut screen, "Most recent boot log lines:", TITLE, &mut row);
    // Fill the rest of the screen with the newest log lines (each wrapped).
    let free = rows.saturating_sub(row);
    let log = crate::klog::tail_lines(log, free);
    let text = core::str::from_utf8(log).unwrap_or("(boot log is not UTF-8)");
    let wrapped: usize = text.lines().map(|l| wrapped_rows(l, columns)).sum();
    let mut skip = wrapped.saturating_sub(free);
    for log_line in text.lines() {
        for chunk in wrap(log_line, columns) {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            line(&mut screen, chunk, LOG, &mut row);
        }
    }
}

/// Integer pixel scale that keeps text readable: 2x from 2560 wide, 3x at 4K+.
pub fn scale_for(width: usize) -> usize {
    match width {
        0..2560 => 1,
        2560..3840 => 2,
        _ => 3,
    }
}

/// `text` cut into pieces of at most `columns` characters (at least one
/// piece, possibly empty).
pub fn wrap(text: &str, columns: usize) -> impl Iterator<Item = &str> {
    let columns = columns.max(1);
    let mut rest = Some(text);
    core::iter::from_fn(move || {
        let current = rest?;
        match current.char_indices().nth(columns) {
            Some((cut, _)) => {
                rest = Some(&current[cut..]);
                Some(&current[..cut])
            }
            None => {
                rest = None;
                Some(current)
            }
        }
    })
}

fn wrapped_rows(text: &str, columns: usize) -> usize {
    wrap(text, columns).count()
}

/// The framebuffer seen at an integer scale: each logical pixel is a
/// `scale` x `scale` block, so the atlas font stays legible on a large mode.
struct Scaled<'a> {
    fb: &'a mut Framebuffer,
    scale: usize,
}

impl Scaled<'_> {
    fn columns(&self) -> usize {
        (self.width().saturating_sub(2 * MARGIN) / font::ADVANCE as usize).max(1)
    }

    fn rows(&self) -> usize {
        self.height().saturating_sub(2 * MARGIN) / font::LINE_HEIGHT as usize
    }

    fn draw_line(&mut self, row: usize, text: &str, color: Color) {
        let top = (MARGIN + row * font::LINE_HEIGHT as usize) as i32;
        let clip = Rect {
            x0: 0,
            y0: 0,
            x1: self.width() as i32,
            y1: self.height() as i32,
        };
        let baseline = top + font::ASCENDER.max(0);
        text::draw_text(self, MARGIN as i32, baseline, text, color, clip);
    }
}

impl Surface for Scaled<'_> {
    fn width(&self) -> usize {
        self.fb.width() / self.scale
    }

    fn height(&self) -> usize {
        self.fb.height() / self.scale
    }

    fn get(&self, x: usize, y: usize) -> Color {
        self.fb.read_pixel(x * self.scale, y * self.scale)
    }

    fn set(&mut self, x: usize, y: usize, color: Color) {
        let s = self.scale;
        self.fb.fill_rect(x * s, y * s, s, s, color);
    }

    /// Not used by the report (text only); images would need scaling.
    fn blit_rgba_region(
        &mut self,
        _rgba: &[u8],
        _src_w: usize,
        _src_h: usize,
        _sx: usize,
        _sy: usize,
        _dx: usize,
        _dy: usize,
        _w: usize,
        _h: usize,
    ) {
    }
}

/// A fixed-size text buffer for formatting without the heap; overflow is cut
/// at a character boundary.
struct FixedText<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> FixedText<N> {
    fn new() -> Self {
        FixedText {
            bytes: [0; N],
            len: 0,
        }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

impl<const N: usize> Write for FixedText<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for ch in s.chars() {
            let mut utf8 = [0u8; 4];
            let encoded = ch.encode_utf8(&mut utf8).as_bytes();
            if self.len + encoded.len() > N {
                return Ok(());
            }
            self.bytes[self.len..self.len + encoded.len()].copy_from_slice(encoded);
            self.len += encoded.len();
        }
        Ok(())
    }
}
