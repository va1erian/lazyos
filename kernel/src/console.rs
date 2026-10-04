//! Framebuffer text console rendering anti-aliased glyphs.

use crate::font::{self, COVERAGE, GLYPHS};
use crate::gfx::{Color, Framebuffer};
use bootloader_api::info::FrameBufferInfo;
use core::fmt;
use spin::Mutex;

static CONSOLE: Mutex<Option<Console>> = Mutex::new(None);
/// The framebuffer as handed over, for the panic screen: it must draw even
/// when a panic struck while [`CONSOLE`] was locked, so it never takes it.
static RAW: spin::Once<(usize, FrameBufferInfo)> = spin::Once::new();
/// The framebuffer the console draws on now: the firmware's, or the mode
/// `display.mode` switched to.
static CURRENT: Mutex<Option<(usize, FrameBufferInfo)>> = Mutex::new(None);

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
    RAW.call_once(|| (base, info));
    *CURRENT.lock() = Some((base, info));
    let console = Console::new(Framebuffer::new(base, info), 1);
    *CONSOLE.lock() = Some(console);
}

/// The framebuffer's virtual base and length in bytes, as handed over.
pub fn framebuffer_span() -> Option<(u64, u64)> {
    RAW.get()
        .map(|&(base, info)| (base as u64, info.byte_len as u64))
}

/// The framebuffer the console draws on now (`(base, byte length)`): the
/// firmware's, or the one the last mode switch made.
pub fn current_framebuffer_span() -> Option<(u64, u64)> {
    let current = x86_64::instructions::interrupts::without_interrupts(|| *CURRENT.lock());
    current
        .or_else(|| RAW.get().copied())
        .map(|(base, info)| (base as u64, info.byte_len as u64))
}

/// A second handle on the whole framebuffer for the panic screen, built
/// without waiting on any lock. Whatever it draws may interleave with a
/// half-finished blit of the code that panicked; the machine is stopping, so
/// that is fine. It is the current mode ([`switch_mode`]) unless that record
/// is locked at the moment of the panic, then the firmware's.
pub fn panic_framebuffer() -> Option<Framebuffer> {
    let current = CURRENT.try_lock().and_then(|current| *current);
    current
        .or_else(|| RAW.get().copied())
        .map(|(base, info)| Framebuffer::new(base, info))
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
        *CURRENT.lock() = Some((base, info));
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

/// [`with_framebuffer`] on the logical screen (`display::logical`): a view
/// at its centring offset, clipped to it, so a blit sized and placed for the
/// logical screen lands centred and cannot touch the borders or run past
/// the framebuffer. On a mode within the cap this is the whole framebuffer.
pub fn with_screen<R>(f: impl FnOnce(&mut Framebuffer) -> R) -> Option<R> {
    let screen = crate::display::logical();
    with_framebuffer(|fb| f(&mut fb.view(screen.x, screen.y, screen.width, screen.height)))
}

/// Paint everything outside the logical screen black (the borders of a
/// reduced screen), once the desktop or the mux takes over from the boot
/// console. A no-op when the logical screen is the whole mode.
pub fn clear_outside_logical() {
    let screen = crate::display::logical();
    with_framebuffer(|fb| {
        let (width, height) = (fb.width(), fb.height());
        if (screen.width, screen.height) == (width, height) {
            return;
        }
        let black = Color::rgb(0, 0, 0);
        let bottom = screen.y + screen.height;
        let right = screen.x + screen.width;
        fb.fill_rect(0, 0, width, screen.y, black);
        fb.fill_rect(0, bottom, width, height.saturating_sub(bottom), black);
        fb.fill_rect(0, screen.y, screen.x, screen.height, black);
        fb.fill_rect(
            right,
            screen.y,
            width.saturating_sub(right),
            screen.height,
            black,
        );
    });
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
