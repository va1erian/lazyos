//! The boot splash: the LazyOS picture and a spinner, painted by the mux in
//! place of the two console panes while a desktop image boots.
//!
//! A desktop boot used to show the `init` and `xuid` log panes (and, between
//! them and the login screen, a black frame). The mux now paints this
//! picture until the compositor binds the display; `F1` swaps it for the
//! panes (the boot log of `init` and `xuid`) and back. Images with no
//! compositor (`LAZYOS_CLI=1`) have nothing to wait for and keep the panes.
//!
//! The picture is `assets/boot/splash.rgb` (480x270, raw RGB, written by
//! `tools/wallpaper/splash.py` from the LazyOS-Night wallpaper): the kernel
//! has no image decoder, so it scales these pixels to cover the screen.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::gfx::{Color, Framebuffer};
use crate::surface::{RgbaBuffer, Surface};

static SPLASH: &[u8] = include_bytes!("../../assets/boot/splash.rgb");
const SRC_W: usize = 480;
const SRC_H: usize = 270;

/// Whether the boot log panes replace the picture (toggled by `F1`).
static SHOW_LOGS: AtomicBool = AtomicBool::new(false);

/// Dots in the spinner ring and ticks (100 Hz) per step of its head.
const DOTS: usize = 8;
const STEP_TICKS: u64 = 10;
/// Unit-circle positions of the dots times 1000, clockwise from the top.
const UNIT: [(i32, i32); DOTS] = [
    (0, -1000),
    (707, -707),
    (1000, 0),
    (707, 707),
    (0, 1000),
    (-707, 707),
    (-1000, 0),
    (-707, -707),
];
const INK: Color = Color::rgb(0xE8, 0xEC, 0xF6);

/// Whether this image boots a compositor, so the picture has a purpose.
pub const fn available() -> bool {
    cfg!(xuid_demo)
}

/// Whether the mux paints the picture now (rather than the log panes).
pub fn active() -> bool {
    available() && !SHOW_LOGS.load(Ordering::Relaxed)
}

/// `F1`: flip between the picture and the log panes.
pub fn toggle_logs() {
    SHOW_LOGS.fetch_xor(true, Ordering::Relaxed);
    crate::task::NEEDS_REDRAW.store(true, Ordering::Relaxed);
}

/// The head of the spinner now (a pure function of the tick count).
pub fn phase(ticks: u64) -> usize {
    ((ticks / STEP_TICKS) % DOTS as u64) as usize
}

/// Paint the picture over the whole of `back`, cropped to cover it.
pub fn paint(back: &mut RgbaBuffer) {
    let (w, h) = (back.width(), back.height());
    if w == 0 || h == 0 || SPLASH.len() < SRC_W * SRC_H * 3 {
        return;
    }
    // The source window that covers the screen, in 16.16 fixed point.
    let (crop_w, crop_h) = if SRC_W * h > SRC_H * w {
        (SRC_H * w / h, SRC_H)
    } else {
        (SRC_W, SRC_W * h / w)
    };
    let (left, top) = ((SRC_W - crop_w) << 15, (SRC_H - crop_h) << 15);
    let step_x = ((crop_w << 16) / w) as u32;
    let step_y = ((crop_h << 16) / h) as u32;
    for y in 0..h {
        let fy = (top as u32).wrapping_add(step_y.wrapping_mul(y as u32));
        for x in 0..w {
            let fx = (left as u32).wrapping_add(step_x.wrapping_mul(x as u32));
            back.set(x, y, sample(fx, fy));
        }
    }
}

/// The picture bilinearly sampled at 16.16 coordinates.
fn sample(fx: u32, fy: u32) -> Color {
    let (x0, y0) = ((fx >> 16) as usize, (fy >> 16) as usize);
    let (x1, y1) = ((x0 + 1).min(SRC_W - 1), (y0 + 1).min(SRC_H - 1));
    let (x0, y0) = (x0.min(SRC_W - 1), y0.min(SRC_H - 1));
    let (ax, ay) = ((fx >> 8) & 0xFF, (fy >> 8) & 0xFF);
    let at = |x: usize, y: usize, c: usize| u32::from(SPLASH[(y * SRC_W + x) * 3 + c]);
    let channel = |c: usize| {
        let top = at(x0, y0, c) * (256 - ax) + at(x1, y0, c) * ax;
        let bottom = at(x0, y1, c) * (256 - ax) + at(x1, y1, c) * ax;
        ((top * (256 - ay) + bottom * ay) >> 16) as u8
    };
    Color::rgb(channel(0), channel(1), channel(2))
}

/// Where the spinner sits on a `w` x `h` screen: the centre of its ring and
/// the ring and dot radii, below the picture's wordmark.
fn geometry(w: usize, h: usize) -> (i32, i32, i32, i32) {
    let ring = (h / 36).max(10) as i32;
    (
        (w / 2) as i32,
        (h * 80 / 100) as i32,
        ring,
        (ring / 4).max(2),
    )
}

/// The rectangle `(x, y, w, h)` the spinner covers, for the mux to restore
/// from its back buffer before drawing the next step.
pub fn spinner_rect(w: usize, h: usize) -> (usize, usize, usize, usize) {
    let (cx, cy, ring, dot) = geometry(w, h);
    let reach = ring + dot + 1;
    (
        (cx - reach) as usize,
        (cy - reach) as usize,
        (2 * reach) as usize,
        (2 * reach) as usize,
    )
}

/// Draw the spinner at `phase` on the screen: the head bright, the dots
/// behind it fading.
pub fn draw_spinner(fb: &mut Framebuffer, phase: usize) {
    let (cx, cy, ring, dot) = geometry(fb.width(), fb.height());
    for (index, (ux, uy)) in UNIT.iter().enumerate() {
        let behind = (phase + DOTS - index) % DOTS;
        let alpha = (255 - behind * 200 / (DOTS - 1)) as u8;
        disc(fb, cx + ux * ring / 1000, cy + uy * ring / 1000, dot, alpha);
    }
}

/// A filled circle of radius `r` at `(x, y)`, one coverage level per pixel
/// (the edge pixel is half covered).
fn disc(fb: &mut Framebuffer, x: i32, y: i32, r: i32, alpha: u8) {
    for py in y - r..=y + r {
        for px in x - r..=x + r {
            let (dx, dy) = (px - x, py - y);
            let d2 = dx * dx + dy * dy;
            if d2 > (r + 1) * (r + 1) || px < 0 || py < 0 {
                continue;
            }
            let coverage = if d2 <= r * r - r { alpha } else { alpha / 2 };
            fb.blend_pixel(px as usize, py as usize, INK, coverage);
        }
    }
}
