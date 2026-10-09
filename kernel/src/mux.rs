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
/// Ticks the kernel task sleeps while a compositor owns the screen (P7):
/// nothing is painted then, and what the loop still runs is either served
/// elsewhere first (device interrupts on IRQ return and the tick, dead slots
/// on every native syscall) or coarse (the 5 s filesystem flusher, the 30 s
/// shutdown watchdog). Waking every 2 ticks for it was 50 wakeups a second
/// on an idle desktop.
const BOUND_IDLE_TICKS: u64 = 25;
const BACKGROUND: Color = Color::rgb(10, 12, 20);
const WINDOW_BG: Color = Color::rgb(16, 18, 30);
const TITLE_BG: Color = Color::rgb(44, 50, 80);
const BORDER: Color = Color::rgb(70, 82, 120);
const BORDER_FOCUS: Color = Color::rgb(120, 200, 140);
const TEXT_COLOR: Color = Color::rgb(205, 210, 225);

/// Run the multiplexer forever.
///
/// It paints the logical screen (`display::logical`), not the whole mode: on
/// a 4K panel a mode-sized back buffer would be 31.6 MiB, twice the kernel
/// heap. The back buffer only exists while the mux owns the screen; it is
/// freed while a compositor is bound (the whole desktop session) and
/// allocated again, fallibly, when the mux takes the screen back.
pub fn run() -> ! {
    let screen = crate::display::logical();
    let (fbw, fbh) = (screen.width, screen.height);
    console::clear_outside_logical();
    let mut back: Option<RgbaBuffer> = None;

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
        // Report each disk's request counters after a burst of I/O, and
        // write cached filesystem data back every few seconds. Both take
        // spin locks (the serial port, the VFS) that syscalls take with
        // interrupts off: holding one here with interrupts on would let the
        // timer, or a wake (P1.1), switch to a task that then spins on it
        // forever with the timer masked (the #382 rule).
        // Each runs as a kernel section, so a long writeback still takes
        // interrupts at its poll points (`arch::irqoff`, `arch::irq_window`).
        without_interrupts(|| crate::arch::irqoff::kernel_section(crate::block::stats::service));
        without_interrupts(|| crate::arch::irqoff::kernel_section(crate::fs::flusher::service));
        // `PERF:` latency lines (LAZYOS_PERF=1 images only).
        crate::perf::service();
        // Serial output a preempted writer left queued (P5).
        crate::serial::service();
        // A bound compositor owns the screen and input: stop painting entirely
        // and park like any idle task. The check also notices a compositor that
        // exited without unbinding, so this mux is always the fallback.
        if crate::display::bound() {
            yielded = true;
            back = None;
            task::idle(task::ticks() + BOUND_IDLE_TICKS);
            continue;
        }
        if yielded {
            yielded = false;
            mouse_prev = None;
            task::NEEDS_REDRAW.store(true, Ordering::Relaxed);
        }
        if back.is_none() {
            back = RgbaBuffer::try_new(fbw, fbh);
            if back.is_none() {
                // The heap is too fragmented right now: nothing to paint with.
                // Try again next frame rather than stopping the kernel task.
                task::idle(task::ticks() + IDLE_TICKS);
                continue;
            }
            task::NEEDS_REDRAW.store(true, Ordering::Relaxed);
        }
        let Some(back) = back.as_mut() else {
            continue;
        };
        if task::NEEDS_REDRAW.swap(false, Ordering::Relaxed) {
            render(back);
            present(back, 0, 0, fbw, fbh);
            // Redraw the cursor, which the presentation just covered.
            mouse_prev = None;
        }
        if let Some((x, y)) = crate::input::mouse::take_moved() {
            if let Some((px, py)) = mouse_prev {
                present(
                    back,
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

    // Collect the windows first: the layout depends on how many there are.
    let mut windows = alloc::vec::Vec::new();
    for index in 1..MAX_TASKS {
        let Some((name, output, done)) = without_interrupts(|| task::snapshot(index)) else {
            continue;
        };
        windows.push((index, name, output, done));
        if windows.len() == MAX_COLUMNS {
            break; // the layout has two columns
        }
    }
    let count = windows.len();
    let window_h = h - PAD * 2 - TITLE_H;
    for (slot, (index, name, output, done)) in windows.iter().enumerate() {
        let (x, window_w) = column(w, count, slot);
        draw_window(
            back,
            x,
            PAD,
            window_w,
            window_h,
            name,
            output,
            *done,
            task::focus() == *index,
        );
    }
}

/// Most windows the multiplexer lays out side by side.
const MAX_COLUMNS: usize = 2;

/// Left edge and width of column `slot` when `count` windows share a screen
/// `screen_w` wide. A lone window (a `LAZYOS_CLI=1` boot runs only `sh`)
/// takes the full width (issue #218); the terminal text is not wrapped by the
/// multiplexer, so its visible columns simply follow the window.
pub(crate) fn column(screen_w: i32, count: usize, slot: usize) -> (i32, i32) {
    let count = count.clamp(1, MAX_COLUMNS) as i32;
    let width = (screen_w - PAD * (count + 1)) / count;
    (PAD + slot as i32 * (width + PAD), width)
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
    let columns = ((w - PAD * 2) / crate::font::ADVANCE as i32).max(1) as usize;
    let rows = wrap_rows(&lines, columns);
    let visible = ((h - TITLE_H - PAD * 2) / crate::font::LINE_HEIGHT).max(1) as usize;
    let start = rows.len().saturating_sub(visible);
    let mut baseline = y + TITLE_H + PAD + crate::font::ASCENDER.max(0);
    for row in &rows[start.min(rows.len())..] {
        let text = core::str::from_utf8(row).unwrap_or("");
        text::draw_text(back, x + PAD, baseline, text, TEXT_COLOR, content_clip);
        baseline += crate::font::LINE_HEIGHT;
    }
}

/// Break each output line into rows of at most `columns` bytes (the font is
/// fixed-advance), so a long log line such as `USBD:XHCI ... handoff=...` is
/// read in full on a PC with no serial port instead of being cut at the
/// window edge. A line is cut on a byte boundary that is also a character
/// boundary; a short line is one row and an empty line stays one empty row.
pub(crate) fn wrap_rows<'a>(lines: &[&'a [u8]], columns: usize) -> alloc::vec::Vec<&'a [u8]> {
    let columns = columns.max(1);
    let mut rows = alloc::vec::Vec::with_capacity(lines.len());
    for line in lines {
        let mut rest = *line;
        while rest.len() > columns {
            let mut cut = columns;
            // Never split a UTF-8 sequence (continuation bytes are 10xxxxxx).
            while cut > 1 && rest[cut] & 0xC0 == 0x80 {
                cut -= 1;
            }
            rows.push(&rest[..cut]);
            rest = &rest[cut..];
        }
        rows.push(rest);
    }
    rows
}

/// Present a rectangle of the back buffer to the logical screen.
fn present(back: &RgbaBuffer, x: usize, y: usize, w: usize, h: usize) {
    console::with_screen(|fb| {
        fb.blit_rgba_region(back.data(), back.width(), back.height(), x, y, x, y, w, h);
    });
}

fn draw_cursor(pos: (i32, i32)) {
    console::with_screen(|fb| cursor::draw(fb, pos.0, pos.1));
}

/// Run a closure with interrupts disabled (safe to take `task`'s lock).
fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    x86_64::instructions::interrupts::without_interrupts(f)
}
