//! A toast's face: the level icon with its arrival flourish, the sender's
//! verified name, the title and body, the progress bar, the action buttons,
//! the close mark and the countdown bar. Everything is laid out by
//! `lazyshell::notify::card`; this only draws it, at the desktop's scale.
//!
//! Plain text and a small icon set only: nothing a sender supplies is
//! interpreted, so a toast cannot imitate shell chrome beyond what its
//! words say, and the header always names the verified sender.

use std::rc::Rc;

use lazymotion::Motion;
use lazyshell::notify::card::{Hit, ACTION_PAD};
use lazyshell::notify::note::Level;
use lazyshell::Rect as ShellRect;
use xui_core::backend::TextStyle;
use xui_core::image::Image;
use xui_core::theme::look;
use xui_core::{draw_icon, Canvas, Dip, Lucide, Point, Rect};

use super::super::ctx::Ctx;
use super::super::theme::{chrome_look, color};
use super::toast::View;

/// The accent of each level (Info follows the theme's selection colour).
const SUCCESS: u32 = 0x3FA66A;
const WARN: u32 = 0xD9A21B;
const ERROR: u32 = 0xD2504B;
/// The left strip's width, design pixels.
const STRIP: i32 = 3;

/// A picture ready to draw.
pub enum Picture {
    /// The level's own icon.
    Level,
    /// An app-named outline.
    Outline(Lucide),
    /// An app's image.
    Image(Rc<Image>),
}

/// A design-pixel rectangle at scale `s`.
fn rect(r: ShellRect, s: i32) -> Rect {
    Rect::new(r.x * s, r.y * s, (r.x + r.w) * s, (r.y + r.h) * s)
}

/// The level's accent colour.
pub fn accent(level: Level, selected: u32) -> u32 {
    match level {
        Level::Info => selected,
        Level::Success => SUCCESS,
        Level::Warn => WARN,
        Level::Error => ERROR,
    }
}

/// Paint the toast in `view`.
pub fn paint(canvas: &mut dyn Canvas, ctx: &Ctx, view: &View) {
    let palette = ctx.theme.borrow().palette();
    let deco = chrome_look(ctx.theme.borrow().is_dark());
    let s = ctx.scale();
    let dpi = canvas.dpi();
    let bounds = canvas.bounds();
    let (bg, text) = (palette.overlay_bg, palette.overlay_text);
    // Reduced motion fades the content in and out in place.
    let sixteenths = (view.frame.fade.clamp(0.0, 1.0) * 16.0).round() as u32;
    let faded = |rgb: u32| uitheme::mix(bg, rgb, sixteenths, 16);
    let ink = faded(text);
    let dim = faded(uitheme::mix(text, bg, 2, 5));
    let accent = faded(accent(view.level, palette.overlay_selected));

    canvas.fill_rect(bounds, color(bg));
    canvas.stroke_rect(bounds, color(palette.overlay_border), s as f32);
    canvas.fill_rect(
        Rect::new(
            bounds.left,
            bounds.top,
            bounds.left + STRIP * s,
            bounds.bottom,
        ),
        color(accent),
    );

    let card = &view.card;
    let flourish = view.frame.flourish(view.level, view.motion);
    paint_icon(canvas, view, rect(card.icon, s), flourish, (s, dpi), accent);

    // The verified sender, then the sender's words.
    let line = |y: i32, x: i32, w: i32, h: i32| {
        Rect::new(
            bounds.left + x * s,
            bounds.top + y * s,
            bounds.left + (x + w) * s,
            bounds.top + (y + h) * s,
        )
    };
    let name_style = TextStyle::new(color(dim), Dip(10.0)).middle().bold();
    canvas.draw_text(
        &card.name.text,
        line(card.name.y, card.text_x, card.text_w, 14),
        &name_style,
    );
    let title_style = TextStyle::new(color(ink), Dip(12.0)).middle().bold();
    for row in &card.title {
        canvas.draw_text(
            &row.text,
            line(row.y, card.text_x, card.text_w, 16),
            &title_style,
        );
    }
    let body_style = TextStyle::new(color(ink), Dip(11.0)).middle();
    for row in &card.body {
        canvas.draw_text(
            &row.text,
            line(row.y, card.text_x, card.text_w, 16),
            &body_style,
        );
    }

    if let (Some(bar), Some(fill)) = (card.progress, view.frame.progress) {
        let track = uitheme::mix(bg, text, 1, 6);
        let area = rect(bar, s);
        canvas.fill_rect(area, color(faded(track)));
        let done = ((area.right - area.left) as f32 * fill.clamp(0.0, 1.0)) as i32;
        canvas.fill_rect(
            Rect::new(area.left, area.top, area.left + done, area.bottom),
            color(accent),
        );
    }

    let hovered = match view.hit {
        Some(Hit::Action(id)) => Some(id),
        _ => None,
    };
    for button in &card.actions {
        let area = rect(button.rect, s);
        let lit = hovered == Some(button.id);
        let face = if lit {
            palette.overlay_selected
        } else {
            uitheme::mix(bg, text, 1, 6)
        };
        look::face(canvas, area, 4.0 * s as f32, color(faded(face)), &deco);
        let label = if lit {
            uitheme::text_on(palette.overlay_selected)
        } else {
            text
        };
        let style = TextStyle::new(color(faded(label)), Dip(11.0))
            .centered()
            .middle();
        let inner = Rect::new(
            area.left + (ACTION_PAD / 2) * s,
            area.top,
            area.right - (ACTION_PAD / 2) * s,
            area.bottom,
        );
        canvas.draw_text(&button.label, inner, &style);
    }

    // The close mark lights up under the pointer.
    let close = rect(card.close, s);
    if view.hit == Some(Hit::Close) {
        canvas.fill_rounded_rect(
            close,
            3.0 * s as f32,
            color(faded(uitheme::mix(bg, text, 1, 6))),
        );
    }
    let mark = Rect::new(
        close.left + 3 * s,
        close.top + 3 * s,
        close.right - 3 * s,
        close.bottom - 3 * s,
    );
    let mark_ink = if view.hit == Some(Hit::Close) {
        ink
    } else {
        dim
    };
    draw_icon(canvas, Lucide::X, mark, color(mark_ink), dpi);

    // While the pointer rests on it, how much of its life is left.
    if let Some(left) = view.frame.countdown {
        let w = ((bounds.right - bounds.left) as f32 * left.clamp(0.0, 1.0)) as i32;
        let h = 2 * s;
        canvas.fill_rect(
            Rect::new(
                bounds.left,
                bounds.bottom - h,
                bounds.left + w,
                bounds.bottom,
            ),
            color(accent),
        );
    }
}

/// The icon with its flourish: the sender's picture, or the level's own.
fn paint_icon(
    canvas: &mut dyn Canvas,
    view: &View,
    area: Rect,
    flourish: lazyshell::notify::frame::Flourish,
    (s, dpi): (i32, u32),
    accent: u32,
) {
    let moved = area.offset(
        (flourish.dx * s as f32).round() as i32,
        (flourish.dy * s as f32).round() as i32,
    );
    match &view.picture {
        Picture::Image(image) => canvas.draw_image(image, moved),
        Picture::Outline(outline) => draw_icon(canvas, *outline, moved, color(accent), dpi),
        Picture::Level if view.level == Level::Success => {
            paint_tick(canvas, moved, s, flourish.tick, accent, view.motion);
        }
        Picture::Level => {
            let outline = lazyicons::from_name(view.level.lucide()).unwrap_or(Lucide::AppWindow);
            draw_icon(canvas, outline, moved, color(accent), dpi);
        }
    }
}

/// The Success icon: a ring and a tick of which `tick` (`0..=1`) is drawn,
/// so it can draw itself.
fn paint_tick(canvas: &mut dyn Canvas, area: Rect, s: i32, tick: f32, ink: u32, motion: Motion) {
    let cx = (area.left + area.right) / 2;
    let cy = (area.top + area.bottom) / 2;
    let radius = ((area.right - area.left) as f32 / 2.0 - 1.0 * s as f32).max(1.0);
    let width = (1.6 * s as f32).max(1.0);
    canvas.stroke_ellipse(Point::new(cx, cy), radius, radius, color(ink), width);
    // The tick's three points as fractions of the ring's box.
    let size = (area.right - area.left) as f32;
    let at = |fx: f32, fy: f32| {
        Point::new(
            area.left + (fx * size).round() as i32,
            area.top + (fy * size).round() as i32,
        )
    };
    let (a, b, c) = (at(0.29, 0.52), at(0.44, 0.67), at(0.72, 0.36));
    let first = dist(a, b);
    let second = dist(b, c);
    let shown = if motion == Motion::Full { tick } else { 1.0 };
    let mut left = shown.clamp(0.0, 1.0) * (first + second);
    let mut part = |from: Point, to: Point, length: f32, left: &mut f32| {
        if *left <= 0.0 {
            return;
        }
        let t = (*left / length).min(1.0);
        let end = Point::new(
            from.x + ((to.x - from.x) as f32 * t).round() as i32,
            from.y + ((to.y - from.y) as f32 * t).round() as i32,
        );
        canvas.draw_line(from, end, color(ink), width);
        *left -= length;
    };
    part(a, b, first, &mut left);
    part(b, c, second, &mut left);
}

fn dist(a: Point, b: Point) -> f32 {
    let (dx, dy) = ((b.x - a.x) as f32, (b.y - a.y) as f32);
    (dx * dx + dy * dy).sqrt()
}
