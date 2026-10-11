//! The FeedView painter (R4): walk only the visible rows, draw cached
//! layouts, never shape, never measure.

use xui_core::backend::{Canvas, Rgba};
use xui_core::geometry::{Point, Rect};
use xui_core::Theme;

use super::{ViewState, BAR_96};
use crate::images::ImageCache;
use crate::prepare::{Piece, Prepared, Tone};

/// Paints the visible rows of `state` on `canvas`, a scroll bar over them.
///
/// Clips are the one expensive thing here: while any is active every draw
/// applies a mask (five rows measured 6 ms a frame under a clip each and
/// 0.7 ms with none). So only the rows that stick out of the view (the
/// first and the last) and rows with a layout for another width are drawn
/// under a clip; every row wholly inside is drawn bare.
pub fn paint(canvas: &mut dyn Canvas, state: &ViewState, theme: &Theme) {
    let bounds = canvas.bounds();
    canvas.clear(theme.background);
    let feed = &state.feed;
    let images = state.images.borrow();
    let content_w = state.content_width();
    for i in feed.visible() {
        let Some(row) = feed.row(i) else { continue };
        let top = bounds.top + feed.screen_top(i);
        let rect = Rect::new(
            bounds.left,
            top,
            bounds.left + state.width,
            top + row.height,
        );
        let sticks_out = rect.top < bounds.top || rect.bottom > bounds.bottom;
        let Some(prepared) = &row.prepared else {
            if sticks_out {
                canvas.push_clip(within(rect, bounds));
            }
            skeleton(canvas, rect, state, theme);
            if sticks_out {
                canvas.pop_clip();
            }
            continue;
        };
        let clipped = sticks_out || prepared.width != content_w;
        if clipped {
            let visible = within(rect, bounds);
            canvas.push_clip(visible);
        }
        draw_row(
            canvas,
            prepared,
            (rect.left, rect.top),
            state.width,
            theme,
            &images,
        );
        if clipped {
            canvas.pop_clip();
        }
    }
    if feed.is_empty() && !state.note.is_empty() {
        // Rare and short: drawn with the canvas's own text path.
        let mut style = xui_core::TextStyle::new(theme.text_secondary, xui_core::Dip(14.0));
        style.align = xui_core::backend::TextAlign::Center;
        style.valign = xui_core::backend::TextVAlign::Middle;
        style.wrap = true;
        canvas.draw_text(&state.note, bounds, &style);
    }
    scroll_bar(canvas, bounds, state, theme);
}

fn colour(theme: &Theme, tone: Tone) -> Rgba {
    Rgba::from(match tone {
        Tone::Primary => theme.text,
        Tone::Secondary => theme.text_secondary,
        Tone::Accent => theme.accent,
    })
}

fn draw_row(
    canvas: &mut dyn Canvas,
    p: &Prepared,
    origin: (i32, i32),
    width: i32,
    theme: &Theme,
    images: &ImageCache,
) {
    let at = |x: i32, y: i32| Point::new(origin.0 + x, origin.1 + y);
    let shift = |r: &Rect| {
        Rect::new(
            origin.0 + r.left,
            origin.1 + r.top,
            origin.0 + r.right,
            origin.1 + r.bottom,
        )
    };
    // Tints first, so the text draws over them.
    for piece in &p.pieces {
        if let Piece::LinkTint { rect } = piece {
            let a = theme.accent;
            canvas.fill_rect_rgba(shift(rect), Rgba::with_alpha(a.r, a.g, a.b, 48));
        }
    }
    for piece in &p.pieces {
        match piece {
            Piece::Text {
                layout,
                at: pos,
                tone,
            } => {
                canvas.draw_layout(&**layout, at(pos.0, pos.1), colour(theme, *tone));
            }
            Piece::Avatar { rect, key } => {
                let r = shift(rect);
                match key.as_ref().and_then(|key| images.get(key)) {
                    // Already a circle, at the box's size: one draw.
                    Some(image) => canvas.draw_image(image, r),
                    None => {
                        let radius = (r.width() / 2) as f32;
                        canvas.fill_ellipse(
                            Point::new(r.left + r.width() / 2, r.top + r.height() / 2),
                            radius,
                            radius,
                            theme.track,
                        );
                    }
                }
            }
            Piece::Media { rect, key, .. } => {
                let r = shift(rect);
                match key.as_ref().and_then(|key| images.get(key)) {
                    Some(image) => canvas.draw_image(image, r),
                    None => canvas.fill_rounded_rect(r, 6.0, theme.track),
                }
            }
            Piece::Frame { rect } => {
                canvas.stroke_rect(shift(rect), theme.border, 1.0);
            }
            Piece::Rule { y } => {
                let y = origin.1 + y;
                canvas.draw_line(
                    Point::new(origin.0, y),
                    Point::new(origin.0 + width, y),
                    theme.border,
                    1.0,
                );
            }
            Piece::LinkTint { .. } => {}
        }
    }
}

/// A row not yet shaped: flat bars at its known height, no animation.
fn skeleton(canvas: &mut dyn Canvas, rect: Rect, state: &ViewState, theme: &Theme) {
    let pad = state.scaled(12);
    let left = rect.left + pad;
    let (avatar, bar) = (state.scaled(48), state.scaled(10));
    canvas.fill_ellipse(
        Point::new(left + avatar / 2, rect.top + pad + avatar / 2),
        (avatar / 2) as f32,
        (avatar / 2) as f32,
        theme.track,
    );
    let x = left + avatar + state.scaled(10);
    let right = rect.right - pad;
    let mut y = rect.top + pad;
    for (i, frac) in [(0, 40), (1, 25), (2, 90), (3, 70)] {
        let w = ((right - x).max(0) * frac) / 100;
        let r = Rect::new(x, y, x + w, y + bar);
        if r.bottom < rect.bottom {
            canvas.fill_rounded_rect(r, 3.0, theme.track);
        }
        y += bar + state.scaled(if i == 1 { 14 } else { 8 });
    }
}

/// The scroll bar's thumb in widget-local pixels: `(top, length)`.
pub(super) fn thumb(state: &ViewState) -> Option<(i32, i32)> {
    let (total, view) = (state.feed.total_height(), state.height);
    if total <= view || view <= 0 {
        return None;
    }
    let min = state.scaled(BAR_96) * 2;
    let len = ((view as i64 * view as i64) / total as i64).max(min as i64) as i32;
    let len = len.min(view);
    let room = (total - view).max(1);
    let at = (state.feed.scroll_top() as i64 * (view - len) as i64 / room as i64) as i32;
    Some((at, len))
}

fn scroll_bar(canvas: &mut dyn Canvas, bounds: Rect, state: &ViewState, theme: &Theme) {
    let Some((at, len)) = thumb(state) else {
        return;
    };
    let bar = state.scaled(BAR_96);
    let track = Rect::new(bounds.right - bar, bounds.top, bounds.right, bounds.bottom);
    canvas.fill_rect_rgba(track, Rgba::from(theme.scrollbar_track));
    let thumb = Rect::new(
        track.left + 2,
        bounds.top + at + 2,
        track.right - 2,
        bounds.top + at + len - 2,
    );
    canvas.fill_rounded_rect(thumb, 3.0, theme.scrollbar);
}

/// `r` cut to `bounds`.
fn within(r: Rect, bounds: Rect) -> Rect {
    Rect::new(
        r.left.max(bounds.left),
        r.top.max(bounds.top),
        r.right.min(bounds.right),
        r.bottom.min(bounds.bottom),
    )
}
