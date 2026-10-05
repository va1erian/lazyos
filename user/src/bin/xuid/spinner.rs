//! The placeholder a window shows before its first buffer: a ring of dots
//! with a bright head that turns, instead of a line of text.
//!
//! The phase is derived from the PIT clock, so every waiting window turns in
//! step and nothing has to remember per-window state; [`Compositor::tick_spinner`]
//! repaints the waiting windows' content each time the phase moves (the main
//! loop parks at most `IDLE_TICKS`, so the ring advances about ten times a
//! second while one is waiting and costs nothing otherwise).

use user::messenger::display::{Canvas, Color, Rect};
use user::sys;

use super::compositor::Compositor;
use super::render::has_pixels;
use super::theme::px;

/// Dots in the ring.
const DOTS: u32 = 8;
/// PIT ticks (100 Hz) per step of the head.
const STEP_TICKS: u64 = 10;
/// Ring radius and dot radius, in design pixels.
const RING_R: i32 = 14;
const DOT_R: i32 = 3;
/// Unit-circle positions of the dots, scaled by 1000 (no float trig in
/// `no_std`): 0 degrees at the top, clockwise in 45-degree steps.
const UNIT: [(i32, i32); DOTS as usize] = [
    (0, -1000),
    (707, -707),
    (1000, 0),
    (707, 707),
    (0, 1000),
    (-707, 707),
    (-1000, 0),
    (-707, -707),
];

/// The head's position now.
pub(super) fn phase() -> u32 {
    ((sys::clock() / STEP_TICKS) % u64::from(DOTS)) as u32
}

/// Paint the spinner centred in `content` over the already-filled `bg`:
/// the head at `phase` in `ink`, the dots behind it fading toward `bg`.
pub(super) fn draw(screen: &mut Canvas, content: Rect, phase: u32, ink: Color, bg: Color, clip: Rect) {
    let (cx, cy) = (content.x + content.w / 2, content.y + content.h / 2);
    let (ring, dot) = (px(RING_R), px(DOT_R));
    for (index, (ux, uy)) in UNIT.iter().enumerate() {
        // 0 for the head, DOTS - 1 for the dot just ahead of it.
        let behind = (phase + DOTS - index as u32) % DOTS;
        let strength = 255 - behind * 200 / (DOTS - 1);
        let color = mix(ink, bg, strength);
        let (x, y) = (cx + ux * ring / 1000, cy + uy * ring / 1000);
        disc(screen, x, y, dot, color, clip);
    }
}

/// `a` over `b` at `alpha` (0..=255).
fn mix(a: Color, b: Color, alpha: u32) -> Color {
    let blend = |a: u8, b: u8| ((a as u32 * alpha + b as u32 * (255 - alpha)) / 255) as u8;
    Color {
        r: blend(a.r, b.r),
        g: blend(a.g, b.g),
        b: blend(a.b, b.b),
    }
}

/// An anti-aliased filled circle of radius `r` at `(x, y)`: each pixel's
/// coverage comes from 4x4 subsamples against the circle.
fn disc(screen: &mut Canvas, x: i32, y: i32, r: i32, color: Color, clip: Rect) {
    let r4 = r * 4;
    for py in y - r..=y + r {
        for px_ in x - r..=x + r {
            let mut hits = 0u32;
            for sy in 0..4 {
                for sx in 0..4 {
                    let dx = (px_ - x) * 4 + sx * 2 - 3;
                    let dy = (py - y) * 4 + sy * 2 - 3;
                    if dx * dx + dy * dy <= r4 * r4 {
                        hits += 1;
                    }
                }
            }
            if hits > 0 {
                let alpha = (hits * 255 / 16) as u8;
                screen.blend_pixel(px_, py, color, alpha, clip);
            }
        }
    }
}

impl Compositor {
    /// Advance the placeholder spinner: when its phase moved, repaint the
    /// content of every visible window still waiting for a buffer.
    pub(super) fn tick_spinner(&mut self) {
        let now = phase();
        if now == self.spinner_phase {
            return;
        }
        self.spinner_phase = now;
        let waiting: alloc::vec::Vec<Rect> = self
            .surfaces
            .iter()
            .filter(|surface| surface.is_window() && !surface.minimized && !has_pixels(surface))
            .map(|surface| surface.content())
            .collect();
        for content in waiting {
            self.repaint(content);
        }
    }
}
