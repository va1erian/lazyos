//! The HUD, drawn with xui's canvas primitives after the 3D view (the view
//! goes first, while no coverage mask exists, so its blit is a row copy):
//! a bevelled Win95-style info panel, the controls, the overhead map with
//! the camera on it, and the loading screen while a course is generated.

use xui_core::backend::{Canvas, TextStyle};
use xui_core::geometry::{Point, Rect};
use xui_core::{Color, Dip};

use crate::game::Game;

const MARGIN: i32 = 10;
const LINE: i32 = 17;

const HELP: [(&str, &str); 11] = [
    ("W A S D, arrows", "fly, turn"),
    ("Space E / C Q", "up / down"),
    ("Shift", "fast"),
    ("Drag left / right", "look / pan"),
    ("Wheel", "flying speed"),
    ("N / P / Home", "next, previous hole, first tee"),
    ("Click the map", "fly there"),
    ("T / R / M", "authentic, resolution, map"),
    ("G / B", "new course, benchmark"),
    ("H", "hide this help"),
    ("Esc", "quit"),
];

/// Window pixels per design pixel.
fn ui_scale(canvas: &dyn Canvas) -> i32 {
    (canvas.dpi() as i32 / 96).max(1)
}

/// Where the overhead map sits: the bottom-right corner.
pub fn minimap_rect(bounds: Rect, size: i32, scale: i32) -> Rect {
    let m = MARGIN * scale;
    Rect::new(
        bounds.right - m - size,
        bounds.bottom - m - size,
        bounds.right - m,
        bounds.bottom - m,
    )
}

/// Paints the whole widget.
pub fn paint(canvas: &mut dyn Canvas, game: &Game) {
    let bounds = canvas.bounds();
    let s = ui_scale(canvas);
    let Some(run) = game.run.as_ref() else {
        let (label, done) = game.loading().unwrap_or(("Ready".into(), 1.0));
        loading(canvas, bounds, &label, done, game.seed(), s);
        return;
    };
    match &run.image {
        Some(image) => {
            let r = Rect::new(
                bounds.left,
                bounds.top,
                bounds.left + image.width() as i32,
                bounds.top + image.height() as i32,
            );
            canvas.draw_image(image, r);
        }
        None => canvas.fill_rect(bounds, Color::hex(0x000000)),
    }
    let course = &run.scene.course;
    let eye = run.flyer.camera.eye;
    let mut lines = vec![
        course.name.clone(),
        format!(
            "{}  par {}  {:.0} m  rating {:.1}  slope {}  quality {:.1}",
            course.archetype.name(),
            course.par,
            course.total_length_m(),
            course.rating,
            course.slope,
            course.quality
        ),
    ];
    let index = course.hole_of.at(eye.x, eye.z) as usize;
    if let Some(hole) = course.holes.get(index) {
        lines.push(format!(
            "Hole {}  par {}  {:.0} m  stroke index {}",
            hole.number, hole.par, hole.length_m, hole.handicap_index
        ));
    }
    let (fps, work) = run.fps();
    let (w, h) = run.resolution();
    let scale = if run.auto_scale {
        format!("auto x{}", run.scale)
    } else {
        format!("x{}", run.scale)
    };
    lines.push(format!("{fps} fps  {work:.1} ms  {w}x{h} {scale}"));
    let ground = run.scene.bake.height_at(eye.x, eye.z);
    lines.push(format!(
        "{:.0} m/s  {:.0} m up",
        run.flyer.speed,
        eye.y - ground
    ));
    if run.authentic {
        lines.push("Authentic mode: the view paints itself in".into());
    }
    if run.benching() {
        lines.push("Benchmark flyover...".into());
    }
    let panel = Rect::new(
        bounds.left + MARGIN * s,
        bounds.top + MARGIN * s,
        bounds.left + MARGIN * s + 330 * s,
        bounds.top + MARGIN * s + (LINE * lines.len() as i32 + 10) * s,
    );
    bevel_panel(canvas, panel);
    text_lines(canvas, panel, &lines, s, true);
    if game.show_help {
        let panel = Rect::new(
            bounds.left + MARGIN * s,
            bounds.bottom - MARGIN * s - (LINE * HELP.len() as i32 + 10) * s,
            bounds.left + MARGIN * s + 330 * s,
            bounds.bottom - MARGIN * s,
        );
        if panel.top > bounds.top + 140 * s {
            bevel_panel(canvas, panel);
            let keys: Vec<String> = HELP.iter().map(|(k, _)| k.to_string()).collect();
            let what: Vec<String> = HELP.iter().map(|(_, w)| w.to_string()).collect();
            text_lines(canvas, panel, &keys, s, false);
            let right = Rect::new(panel.left + 118 * s, panel.top, panel.right, panel.bottom);
            text_lines(canvas, right, &what, s, false);
        }
    }
    if game.show_map {
        map(canvas, run, bounds, s);
    }
}

/// The overhead map with the camera's position and heading.
fn map(canvas: &mut dyn Canvas, run: &crate::game::Running, bounds: Rect, s: i32) {
    let size = run.minimap.width() as i32;
    let r = minimap_rect(bounds, size, s);
    if r.left < bounds.left + 340 * s || r.top < bounds.top {
        return;
    }
    canvas.draw_image(&run.minimap, r);
    canvas.stroke_rect(r, Color::hex(0x202020), 1.0);
    let k = size as f32 / run.scene.bake.size as f32;
    let cam = run.flyer.camera;
    let at = Point::new(
        r.left + (cam.eye.x * k) as i32,
        r.top + (cam.eye.z * k) as i32,
    );
    if !(r.left..r.right).contains(&at.x) || !(r.top..r.bottom).contains(&at.y) {
        return;
    }
    let half = cam.fov * 0.5;
    for side in [-half, half] {
        let yaw = cam.yaw + side;
        let tip = Point::new(
            at.x + (yaw.sin() * 26.0 * s as f32) as i32,
            at.y - (yaw.cos() * 26.0 * s as f32) as i32,
        );
        canvas.draw_line(at, tip, Color::hex(0xFFFFFF), 1.0);
    }
    canvas.fill_ellipse(at, 3.0 * s as f32, 3.0 * s as f32, Color::hex(0xFFE030));
}

/// A grey Win95 panel with a light top-left and dark bottom-right edge.
pub fn bevel_panel(c: &mut dyn Canvas, r: Rect) {
    c.fill_rect(r, Color::hex(0xC0C0C0));
    let (l, t, rt, b) = (r.left, r.top, r.right - 1, r.bottom - 1);
    c.draw_line(
        Point::new(l, t),
        Point::new(rt, t),
        Color::hex(0xFFFFFF),
        1.0,
    );
    c.draw_line(
        Point::new(l, t),
        Point::new(l, b),
        Color::hex(0xFFFFFF),
        1.0,
    );
    c.draw_line(
        Point::new(l, b),
        Point::new(rt, b),
        Color::hex(0x404040),
        1.0,
    );
    c.draw_line(
        Point::new(rt, t),
        Point::new(rt, b),
        Color::hex(0x404040),
        1.0,
    );
}

fn text_lines(canvas: &mut dyn Canvas, panel: Rect, lines: &[String], s: i32, first_bold: bool) {
    for (i, line) in lines.iter().enumerate() {
        let top = panel.top + (5 + LINE * i as i32) * s;
        let rect = Rect::new(panel.left + 8 * s, top, panel.right - 6 * s, top + LINE * s);
        let mut style = TextStyle::new(Color::hex(0x101010), Dip(12.0));
        if i == 0 && first_bold {
            style = style.bold();
        }
        canvas.draw_text(line, rect, &style);
    }
}

/// While the course is generated: the stage and a progress bar.
fn loading(canvas: &mut dyn Canvas, bounds: Rect, label: &str, done: f32, seed: u64, s: i32) {
    canvas.fill_rect(bounds, Color::hex(0x1E4620));
    let (cx, cy) = (
        (bounds.left + bounds.right) / 2,
        (bounds.top + bounds.bottom) / 2,
    );
    let panel = Rect::new(cx - 190 * s, cy - 56 * s, cx + 190 * s, cy + 56 * s);
    bevel_panel(canvas, panel);
    let title = Rect::new(
        panel.left + 12 * s,
        panel.top + 10 * s,
        panel.right - 12 * s,
        panel.top + 34 * s,
    );
    canvas.draw_text(
        "LazyGolf",
        title,
        &TextStyle::new(Color::hex(0x103010), Dip(18.0)).bold(),
    );
    let line = Rect::new(
        panel.left + 12 * s,
        panel.top + 38 * s,
        panel.right - 12 * s,
        panel.top + 56 * s,
    );
    let text = format!("{label}...  (course #{seed})");
    canvas.draw_text(
        &text,
        line,
        &TextStyle::new(Color::hex(0x101010), Dip(12.0)),
    );
    let bar = Rect::new(
        panel.left + 12 * s,
        panel.bottom - 30 * s,
        panel.right - 12 * s,
        panel.bottom - 14 * s,
    );
    canvas.fill_rect(bar, Color::hex(0xFFFFFF));
    let filled = bar.left + ((bar.right - bar.left) as f32 * done.clamp(0.0, 1.0)) as i32;
    canvas.fill_rect(
        Rect::new(bar.left, bar.top, filled, bar.bottom),
        Color::hex(0x000080),
    );
    canvas.stroke_rect(bar, Color::hex(0x404040), 1.0);
}
