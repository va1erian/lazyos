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
}

impl Console {
    fn new(fb: Framebuffer) -> Self {
        let mut console = Console {
            fb,
            pen_x: MARGIN,
            baseline: font::ASCENDER.max(0) as usize,
            fg: FOREGROUND,
            bg: BACKGROUND,
        };
        console.fb.clear(console.bg);
        console
    }

    fn bottom_margin(&self) -> usize {
        font::DESCENDER.unsigned_abs() as usize
    }
    fn newline(&mut self) {
        self.pen_x = MARGIN;
        let next = self.baseline + font::LINE_HEIGHT as usize;
        if next + self.bottom_margin() > self.fb.height() {
            self.fb.scroll_up(font::LINE_HEIGHT as usize, self.bg);
        } else {
            self.baseline = next;
        }
    }

    /// Move back one cell and erase it with the background colour.
    fn backspace(&mut self) {
        if self.pen_x < MARGIN + font::ADVANCE as usize {
            return;
        }
        self.pen_x -= font::ADVANCE as usize;
        let top = self.baseline.saturating_sub(font::ASCENDER.max(0) as usize);
        for y in top..top + font::LINE_HEIGHT as usize {
            for x in self.pen_x..self.pen_x + font::ADVANCE as usize {
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
        let origin_x = self.pen_x as i32 + glyph.left;
        let origin_y = self.baseline as i32 + glyph.top;

        for row in 0..gh {
            let y = origin_y + row as i32;
            if y < 0 || y as usize >= self.fb.height() {
                continue;
            }
            for col in 0..gw {
                let alpha = coverage[row * gw + col];
                if alpha == 0 {
                    continue;
                }
                let x = origin_x + col as i32;
                if x < 0 || x as usize >= self.fb.width() {
                    continue;
                }
                self.fb.blend_pixel(x as usize, y as usize, self.fg, alpha);
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
                    self.pen_x += font::ADVANCE as usize;
                    if self.pen_x + font::ADVANCE as usize + MARGIN > self.fb.width() {
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
    let console = Console::new(Framebuffer::new(base, info));
    *CONSOLE.lock() = Some(console);
}

/// The framebuffer's virtual base and length in bytes, as handed over.
pub fn framebuffer_span() -> Option<(u64, u64)> {
    RAW.get()
        .map(|&(base, info)| (base as u64, info.byte_len as u64))
}

/// A second handle on the whole framebuffer for the panic screen, built
/// without any lock. Whatever it draws may interleave with a half-finished
/// blit of the code that panicked; the machine is stopping, so that is fine.
pub fn panic_framebuffer() -> Option<Framebuffer> {
    RAW.get().map(|&(base, info)| Framebuffer::new(base, info))
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
