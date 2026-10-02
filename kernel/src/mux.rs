//! Terminal multiplexer: the kernel task (slot 0).
//!
//! Renders one window per user task and lets `Tab` move keyboard focus. It never
//! exits; the timer preempts it so the user tasks run.

use core::sync::atomic::Ordering;

use crate::console;
use crate::cursor;
use crate::gfx::Color;
use crate::surface::{RgbaBuffer, Surface};
use crate::task::{self, MAX_TASKS};
use crate::text;

const TITLE_H: i32 = 26;
const PAD: i32 = 8;
/// Ticks the multiplexer sleeps between frames (100 Hz PIT): two ticks keep
/// repaint and cursor latency at or below 20 ms while leaving the rest of the
/// window to user tasks (issue #58).
const IDLE_TICKS: u64 = 2;
const BACKGROUND: Color = Color::rgb(10, 12, 20);
const WINDOW_BG: Color = Color::rgb(16, 18, 30);
const TITLE_BG: Color = Color::rgb(44, 50, 80);
const BORDER: Color = Color::rgb(70, 82, 120);
const BORDER_FOCUS: Color = Color::rgb(120, 200, 140);
const TEXT_COLOR: Color = Color::rgb(205, 210, 225);

/// Run the multiplexer forever.
pub fn run() -> ! {
    let (fbw, fbh) =
        console::with_framebuffer(|fb| (fb.width(), fb.height())).unwrap_or((1280, 720));
    let mut back = RgbaBuffer::new(fbw, fbh);

    let mut mouse_prev: Option<(i32, i32)> = None;
    // Whether a compositor owned the display on the previous iteration, so the
    // mux can repaint from scratch when it takes the screen back (issue #113).
    let mut yielded = false;
    loop {
        // Reclaim task slots the scheduler flagged (issue #133) before any
        // window work: interrupts off, so the drop of a dead task's buffers
        // cannot be preempted by the timer while it holds the task table.
        without_interrupts(task::reclaim_pending);
        // Post interrupt messages for claimed device lines and expire ack
        // deadlines (issue #240); the ISR only records that a line fired.
        crate::dev::intx::service();
        // An orderly shutdown that never reached `power` (docs/shutdown.md):
        // past the armed deadline the kernel forces the stop itself.
        crate::process::power::watchdog::service();
        // Report each disk's request counters after a burst of I/O.
        crate::block::stats::service();
        // Write cached filesystem data back every few seconds.
        crate::fs::flusher::service();
        // A bound compositor owns the screen and input: stop painting entirely
        // and park like any idle task. The check also notices a compositor that
        // exited without unbinding, so this mux is always the fallback.
        if crate::display::bound() {
            yielded = true;
            task::idle(task::ticks() + IDLE_TICKS);
            continue;
        }
        if yielded {
            yielded = false;
            mouse_prev = None;
            task::NEEDS_REDRAW.store(true, Ordering::Relaxed);
        }
        if task::NEEDS_REDRAW.swap(false, Ordering::Relaxed) {
            render(&mut back);
            present(&back, 0, 0, fbw, fbh);
            // Redraw the cursor, which the presentation just covered.
            mouse_prev = None;
        }
        if let Some((x, y)) = crate::input::mouse::take_moved() {
            if let Some((px, py)) = mouse_prev {
                present(
                    &back,
                    px.max(0) as usize,
                    py.max(0) as usize,
                    cursor::WIDTH as usize,
                    cursor::HEIGHT as usize,
                );
            }
            draw_cursor((x, y));
            mouse_prev = Some((x, y));
        }
        // Park until the next frame slot. The mux is an `Interactive` task:
        // sleeping between frames is what bounds its CPU share, so it cannot
        // starve user tasks that busy-wait in `hlt` (native `read_char`).
        task::idle(task::ticks() + IDLE_TICKS);
    }
}

/// Paint the whole screen (background + every task window).
fn render(back: &mut RgbaBuffer) {
    let (w, h) = (back.width() as i32, back.height() as i32);
    back.fill_rect(0, 0, w, h, BACKGROUND);

    // Lay the user windows out side by side.
    let mut slot = 0;
    let window_w = (w - PAD * 3) / 2;
    let window_h = h - PAD * 2 - TITLE_H;
    for index in 1..MAX_TASKS {
        let Some((name, output, done)) = without_interrupts(|| task::snapshot(index)) else {
            continue;
        };
        let x = PAD + slot * (window_w + PAD);
        let y = PAD;
        draw_window(
            back,
            x,
            y,
            window_w,
            window_h,
            name,
            &output,
            done,
            task::focus() == index,
        );
        slot += 1;
        if slot == 2 {
            break; // the layout has two columns
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_window(
    back: &mut RgbaBuffer,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    title: &str,
    output: &[u8],
    done: bool,
    focused: bool,
) {
    let border = if focused { BORDER_FOCUS } else { BORDER };
    back.fill_rect(x, y, x + w, y + h, WINDOW_BG);
    back.fill_rect(x, y, x + w, y + TITLE_H, TITLE_BG);
    // Border and title separator (1px lines).
    back.fill_rect(x, y, x + w, y + 1, border);
    back.fill_rect(x, y + h - 1, x + w, y + h, border);
    back.fill_rect(x, y, x + 1, y + h, border);
    back.fill_rect(x + w - 1, y, x + w, y + h, border);
    back.fill_rect(x, y + TITLE_H, x + w, y + TITLE_H + 1, border);

    let clip = text::Rect {
        x0: x + 1,
        y0: y + 1,
        x1: x + w - 1,
        y1: y + h - 1,
    };
    let label = if done {
        alloc::format!("{title} [exited]")
    } else if focused {
        alloc::format!("{title} *")
    } else {
        alloc::string::String::from(title)
    };
    text::draw_text(back, x + PAD, y + TITLE_H - 8, &label, TEXT_COLOR, clip);

    // Show the tail of the output that fits.
    let content_clip = text::Rect {
        x0: x + PAD,
        y0: y + TITLE_H + 4,
        x1: x + w - PAD,
        y1: y + h - PAD,
    };
    let mut lines: alloc::vec::Vec<&[u8]> = output.split(|&b| b == b'\n').collect();
    if lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    let visible = ((h - TITLE_H - PAD * 2) / crate::font::LINE_HEIGHT).max(1) as usize;
    let start = lines.len().saturating_sub(visible);
    let mut baseline = y + TITLE_H + PAD + crate::font::ASCENDER.max(0);
    for line in &lines[start.min(lines.len())..] {
        let text = core::str::from_utf8(line).unwrap_or("");
        text::draw_text(back, x + PAD, baseline, text, TEXT_COLOR, content_clip);
        baseline += crate::font::LINE_HEIGHT;
    }
}

/// Present a rectangle of the back buffer to the live framebuffer.
fn present(back: &RgbaBuffer, x: usize, y: usize, w: usize, h: usize) {
    console::with_framebuffer(|fb| {
        fb.blit_rgba_region(back.data(), back.width(), back.height(), x, y, x, y, w, h);
    });
}

fn draw_cursor(pos: (i32, i32)) {
    console::with_framebuffer(|fb| cursor::draw(fb, pos.0, pos.1));
}

/// Run a closure with interrupts disabled (safe to take `task`'s lock).
fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(f)
}
