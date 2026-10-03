//! Framebuffer text console rendering anti-aliased glyphs.

use crate::font::{self, COVERAGE, GLYPHS};
use crate::gfx::{Color, Framebuffer};
use bootloader_api::info::FrameBufferInfo;
use core::fmt;
use spin::Mutex;

static CONSOLE: Mutex<Option<Console>> = Mutex::new(None);

const FOREGROUND: Color = Color::rgb(0xE8, 0xE8, 0xF0);
const BACKGROUND: Color = Color::rgb(0x0D, 0x0F, 0x17);
/// Left/right padding around the text area, in pixels.
const MARGIN: usize = 16;

struct Console {
    fb: Framebuffer,
    pen_x: usize,
    baseline: usize,
    fg: Color,
    bg: Color,
    /// Every glyph pixel is drawn as a `scale x scale` block (HiDPI,
    /// docs/hidpi-plan.md): crisp at 2x, and the atlas stays one size.
    scale: usize,
}

impl Console {
    fn new(fb: Framebuffer, scale: usize) -> Self {
        let mut console = Console {
            fb,
            pen_x: 0,
            baseline: 0,
            fg: FOREGROUND,
            bg: BACKGROUND,
            scale: scale.max(1),
        };
        console.home();
        console.fb.clear(console.bg);
        console
    }

    /// Put the pen at the top-left text position for the current scale.
    fn home(&mut self) {
        self.pen_x = self.margin();
        self.baseline = font::ASCENDER.max(0) as usize * self.scale;
    }

    fn margin(&self) -> usize {
        MARGIN * self.scale
    }
    fn advance(&self) -> usize {
        font::ADVANCE as usize * self.scale
    }
    fn line_height(&self) -> usize {
        font::LINE_HEIGHT as usize * self.scale
    }
    fn bottom_margin(&self) -> usize {
        font::DESCENDER.unsigned_abs() as usize * self.scale
    }
    fn newline(&mut self) {
        self.pen_x = self.margin();
        let next = self.baseline + self.line_height();
        if next + self.bottom_margin() > self.fb.height() {
            self.fb.scroll_up(self.line_height(), self.bg);
        } else {
            self.baseline = next;
        }
    }

    /// Move back one cell and erase it with the background colour.
    fn backspace(&mut self) {
        if self.pen_x < self.margin() + self.advance() {
            return;
        }
        self.pen_x -= self.advance();
        let top = self
            .baseline
            .saturating_sub(font::ASCENDER.max(0) as usize * self.scale);
        for y in top..top + self.line_height() {
            for x in self.pen_x..self.pen_x + self.advance() {
                self.fb.write_pixel(x, y, self.bg);
            }
        }
    }

    fn draw_glyph(&mut self, ch: char) {
        let code = ch as u32;
        if code < font::FIRST_CHAR as u32 || code > font::LAST_CHAR as u32 {
            return;
        }
        let glyph = &GLYPHS[(code - font::FIRST_CHAR as u32) as usize];
        let (gw, gh) = (glyph.width as usize, glyph.height as usize);
        if gw == 0 || gh == 0 {
            return;
        }
        let start = glyph.offset as usize;
        let coverage = &COVERAGE[start..start + gw * gh];
        let scale = self.scale as i32;
        let origin_x = self.pen_x as i32 + glyph.left * scale;
        let origin_y = self.baseline as i32 + glyph.top * scale;

        for row in 0..gh {
            for col in 0..gw {
                let alpha = coverage[row * gw + col];
                if alpha == 0 {
                    continue;
                }
                let x = origin_x + col as i32 * scale;
                let y = origin_y + row as i32 * scale;
                self.blend_block(x, y, alpha);
            }
        }
    }

    /// Blend one `scale x scale` block of glyph coverage, clipped.
    fn blend_block(&mut self, x: i32, y: i32, alpha: u8) {
        let scale = self.scale as i32;
        let (width, height) = (self.fb.width() as i32, self.fb.height() as i32);
        for py in y.max(0)..(y + scale).min(height) {
            for px in x.max(0)..(x + scale).min(width) {
                self.fb
                    .blend_pixel(px as usize, py as usize, self.fg, alpha);
            }
        }
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for ch in s.chars() {
            match ch {
                '\n' => self.newline(),
                '\r' => {}
                '\u{8}' => self.backspace(),
                _ => {
                    self.draw_glyph(ch);
                    self.pen_x += self.advance();
                    if self.pen_x + self.advance() + self.margin() > self.fb.width() {
                        self.newline();
                    }
                }
            }
        }
        Ok(())
    }
}

/// Initialise the global console over the bootloader-provided framebuffer.
pub fn init(base: usize, info: FrameBufferInfo) {
    let console = Console::new(Framebuffer::new(base, info), 1);
    *CONSOLE.lock() = Some(console);
}

/// Swap the framebuffer for a new mode (docs/hidpi-plan.md, D1). `switch`
/// reprograms the adapter while the console lock is held with interrupts
/// off, so nothing draws on the old geometry meanwhile; on success the
/// console restarts, cleared, on the returned framebuffer at the same scale.
pub fn switch_mode<E>(
    switch: impl FnOnce() -> Result<(usize, FrameBufferInfo), E>,
) -> Result<FrameBufferInfo, E> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut guard = CONSOLE.lock();
        let scale = guard.as_ref().map_or(1, |console| console.scale);
        let (base, info) = switch()?;
        *guard = Some(Console::new(Framebuffer::new(base, info), scale));
        Ok(info)
    })
}

/// Draw text at `scale` from now on; the screen is cleared so no line mixes
/// two sizes.
pub fn set_scale(scale: usize) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(console) = CONSOLE.lock().as_mut() {
            if console.scale != scale.max(1) {
                console.scale = scale.max(1);
                console.home();
                console.fb.clear(console.bg);
            }
        }
    })
}

/// The console's current scale (a test reads it to restore it).
#[cfg(lazyos_tests)]
pub fn scale() -> usize {
    x86_64::instructions::interrupts::without_interrupts(|| {
        CONSOLE.lock().as_ref().map_or(1, |console| console.scale)
    })
}

/// Whether the console lock is held right now (the NMI hang report, issue #382).
pub fn locked() -> bool {
    CONSOLE.is_locked()
}

/// Run a closure with mutable access to the underlying framebuffer, e.g. to
/// blit a rendered image.
///
/// The lock is held with interrupts off (issue #382). The kernel mux blits
/// from a preemptible context while `present` (syscall 12) blits with
/// interrupts off: a tick that preempted the mux mid-blit handed the CPU to
/// a compositor that then spun on this lock forever, with the timer masked,
/// so the mux never ran again to release it. Holding it with interrupts off
/// everywhere makes a preempted holder impossible on this single CPU.
pub fn with_framebuffer<R>(f: impl FnOnce(&mut Framebuffer) -> R) -> Option<R> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        CONSOLE.lock().as_mut().map(|console| f(&mut console.fb))
    })
}

/// Test hook: write text through the console, at its current scale.
#[cfg(lazyos_tests)]
pub fn write_for_test(text: &str) {
    use fmt::Write as _;
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(console) = CONSOLE.lock().as_mut() {
            let _ = console.write_str(text);
        }
    })
}
