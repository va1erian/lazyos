//! tiny-skia demo: render an anti-aliased 2D scene on the CPU into a `Pixmap`,
//! then blit it to the bootloader framebuffer.
//!
//! `tiny-skia` is used with `default-features = false, features = ["no-std-float"]`
//! so it runs in the kernel; it relies on the heap from the `mem` module.

use crate::console;
use crate::gfx::Framebuffer;
use spin::Mutex;
use tiny_skia::{BlendMode, FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

/// The rendered background scene, retained so windows can restore regions of
/// the screen underneath them (dirty-rectangle compositing).
static BACKGROUND: Mutex<Option<Pixmap>> = Mutex::new(None);

/// Render the demo scene and present it on the screen.
pub fn render_demo() {
    let _ = console::with_framebuffer(|fb| {
        crate::serial_println!("skia: rendering {}x{}", fb.width(), fb.height());
        match draw(fb.width() as u32, fb.height() as u32) {
            Some(pixmap) => {
                crate::serial_println!("skia: pixmap ready, blitting");
                let t = ts();
                blit(fb, &pixmap);
                crate::serial_println!("skia: blit done ({} cyc)", ts() - t);
                *BACKGROUND.lock() = Some(pixmap);
            }
            None => crate::serial_println!("skia: draw returned None"),
        }
    });
}

/// Copy a rectangle of the retained background back onto the framebuffer.
pub fn restore_region(fb: &mut Framebuffer, x: usize, y: usize, w: usize, h: usize) {
    if let Some(bg) = BACKGROUND.lock().as_ref() {
        fb.blit_rgba_region(
            bg.data(),
            bg.width() as usize,
            bg.height() as usize,
            x,
            y,
            x,
            y,
            w,
            h,
        );
    }
}

fn draw(w: u32, h: u32) -> Option<Pixmap> {
    let mut pm = Pixmap::new(w, h)?;
    let (wf, hf) = (w as f32, h as f32);
    let t0 = ts();

    // Banded vertical backdrop. A full-screen gradient *shader* is
    // pathologically slow in this no_std tiny-skia build (~12k cycles/pixel),
    // whereas solid fills are fast, so we approximate the gradient with bands.
    backdrop(&mut pm, wf, hf)?;
    let t1 = ts();
    crate::serial_println!("skia: backdrop {} cyc", t1 - t0);

    // A soft "sun" built from concentric translucent circles.
    let cx = wf * 0.5;
    let cy = hf * 0.40;
    for i in (1..=14).rev() {
        let radius = hf * 0.28 * (i as f32 / 14.0);
        let alpha = (14 + (14 - i) * 12) as u8;
        let path = PathBuilder::from_circle(cx, cy, radius)?;
        let mut sun = Paint::default();
        sun.anti_alias = true;
        sun.set_color_rgba8(255, 196, 96, alpha);
        pm.fill_path(&path, &sun, FillRule::Winding, Transform::identity(), None);
    }

    // Overlapping translucent circles (alpha blending).
    let dots = [
        (0.16f32, (239u8, 71u8, 111u8)),
        (0.39, (255, 209, 102)),
        (0.61, (6, 214, 160)),
        (0.84, (17, 138, 178)),
    ];
    for (fx, (r, g, b)) in dots {
        let path = PathBuilder::from_circle(wf * fx, hf * 0.72, hf * 0.15)?;
        let mut dot = Paint::default();
        dot.anti_alias = true;
        dot.set_color_rgba8(r, g, b, 210);
        pm.fill_path(&path, &dot, FillRule::Winding, Transform::identity(), None);
    }

    // A stroked zigzag near the bottom (rounded joins/caps).
    let mut pb = PathBuilder::new();
    pb.move_to(0.0, hf * 0.88);
    let segments = 12;
    for i in 1..=segments {
        let x = wf * i as f32 / segments as f32;
        let y = if i % 2 == 0 { hf * 0.83 } else { hf * 0.93 };
        pb.line_to(x, y);
    }
    let zigzag = pb.finish()?;
    let t2 = ts();
    crate::serial_println!("skia: sun+dots {} cyc", t2 - t1);
    let mut line_paint = Paint::default();
    line_paint.anti_alias = true;
    line_paint.set_color_rgba8(236, 236, 255, 220);
    let mut stroke = Stroke::default();
    stroke.width = 4.0;
    pm.stroke_path(&zigzag, &line_paint, &stroke, Transform::identity(), None);
    crate::serial_println!("skia: zigzag {} cyc", ts() - t2);

    Some(pm)
}

fn ts() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Fill the pixmap with a vertical gradient approximated by solid bands.
fn backdrop(pm: &mut Pixmap, w: f32, h: f32) -> Option<()> {
    const BANDS: u32 = 96;
    let top = (9u32, 12, 34);
    let mid = (32u32, 16, 68);
    let bottom = (6u32, 42, 54);
    let mut paint = Paint::default();
    paint.anti_alias = false;
    // Opaque bands: replace the destination instead of blending over it.
    paint.blend_mode = BlendMode::Source;
    for i in 0..BANDS {
        let t = i as f32 / (BANDS - 1) as f32;
        // Two-segment colour ramp: top -> mid (t<0.5) -> bottom.
        let (a, b, s) = if t < 0.5 {
            (top, mid, t * 2.0)
        } else {
            (mid, bottom, (t - 0.5) * 2.0)
        };
        let lerp = |x: u32, y: u32| (x as f32 + (y as f32 - x as f32) * s) as u8;
        paint.set_color_rgba8(lerp(a.0, b.0), lerp(a.1, b.1), lerp(a.2, b.2), 255);
        let y = h * i as f32 / BANDS as f32;
        let height = h / BANDS as f32 + 1.0; // +1 to avoid seams between bands
        pm.fill_rect(
            Rect::from_xywh(0.0, y, w, height)?,
            &paint,
            Transform::identity(),
            None,
        );
    }
    Some(())
}

/// Copy a premultiplied-RGBA pixmap onto the framebuffer (which may be BGR).
fn blit(fb: &mut Framebuffer, pm: &Pixmap) {
    fb.blit_rgba(pm.data(), pm.width() as usize, pm.height() as usize);
}
