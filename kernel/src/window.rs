//! A small draggable, scrollable window rendered with tiny-skia chrome and
//! text drawn from the bitmap atlas.

use crate::console;
use crate::font;
use crate::gfx::Color;
use crate::text;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use tiny_skia::{BlendMode, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

const TITLE_H: i32 = 26;
const PAD: i32 = 10;
const LINE_H: i32 = font::LINE_HEIGHT;
const TITLE: &str = "LazyOS - document.txt";

/// A window with a title bar and a scrollable text body.
pub struct Window {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub scroll: usize,
    lines: Vec<&'static str>,
    chrome: Pixmap,
}

impl Window {
    /// Create a window with synthetic document content.
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Option<Self> {
        let mut lines = Vec::new();
        let header: &'static str = Box::leak(format!("{TITLE} - {} lines", 400).into_boxed_str());
        lines.push(header);
        let blank: &'static str = Box::leak(String::from(" ").into_boxed_str());
        lines.push(blank);
        for i in 1..=400 {
            let line: &'static str = Box::leak(
                format!(
                    "{i:03}  The quick brown fox jumps over the lazy dog 0123456789 \
                     -- page up/down scrolls, arrows move the window"
                )
                .into_boxed_str(),
            );
            lines.push(line);
        }

        Some(Window {
            x,
            y,
            w,
            h,
            scroll: 0,
            lines,
            chrome: build_chrome(w, h)?,
        })
    }

    /// Number of text lines that fit in the content area.
    pub fn visible_lines(&self) -> usize {
        ((self.h - TITLE_H - PAD * 2) / LINE_H).max(1) as usize
    }

    pub fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.visible_lines())
    }

    fn page(&self) -> usize {
        self.visible_lines().saturating_sub(1).max(1)
    }

    /// Scroll by one page. Returns true if the offset changed.
    pub fn scroll_page(&mut self, direction: i32) -> bool {
        let target = self.scroll as i32 + direction * self.page() as i32;
        let clamped = target.clamp(0, self.max_scroll() as i32) as usize;
        let changed = clamped != self.scroll;
        self.scroll = clamped;
        changed
    }

    pub fn scroll_to(&mut self, line: usize) -> bool {
        let clamped = line.min(self.max_scroll());
        let changed = clamped != self.scroll;
        self.scroll = clamped;
        changed
    }

    /// Move the window, keeping it fully on a `fbw * fbh` screen.
    pub fn move_to(&mut self, nx: i32, ny: i32, fbw: i32, fbh: i32) {
        self.x = nx.clamp(0, (fbw - self.w).max(0));
        self.y = ny.clamp(0, (fbh - self.h).max(0));
    }

    /// Draw the window (chrome + title + visible lines) at its position.
    pub fn render(&self) {
        let _ = console::with_framebuffer(|fb| {
            fb.blit_rgba_at(
                self.chrome.data(),
                self.w as usize,
                self.h as usize,
                self.x as usize,
                self.y as usize,
            );

            let whole = text::Rect {
                x0: self.x + 1,
                y0: self.y + 1,
                x1: self.x + self.w - 1,
                y1: self.y + self.h - 1,
            };
            text::draw_text(
                fb,
                self.x + PAD,
                self.y + TITLE_H - 8,
                TITLE,
                Color::rgb(235, 238, 255),
                whole,
            );

            // Scrollbar on the right edge.
            let track_x0 = self.x + self.w - 8;
            let track_y0 = self.y + TITLE_H + 4;
            let track_h = (self.h - TITLE_H - 8).max(1);
            let total = self.lines.len().max(1);
            let thumb_h = (track_h * self.visible_lines() as i32 / total as i32).max(12);
            let thumb_y = track_y0
                + (track_h - thumb_h) * self.scroll as i32 / self.max_scroll().max(1) as i32;
            fb.fill_rect(
                track_x0,
                track_y0,
                self.x + self.w - 4,
                track_y0 + track_h,
                Color::rgb(30, 34, 50),
            );
            fb.fill_rect(
                track_x0 + 1,
                thumb_y,
                self.x + self.w - 5,
                thumb_y + thumb_h,
                Color::rgb(120, 150, 220),
            );

            let content = text::Rect {
                x0: self.x + PAD,
                y0: self.y + TITLE_H + 2,
                x1: self.x + self.w - 12,
                y1: self.y + self.h - PAD,
            };
            let mut baseline = self.y + TITLE_H + PAD + font::ASCENDER.max(0);
            for i in 0..self.visible_lines() {
                let index = self.scroll + i;
                if index >= self.lines.len() {
                    break;
                }
                text::draw_text(
                    fb,
                    self.x + PAD,
                    baseline,
                    self.lines[index],
                    Color::rgb(205, 210, 225),
                    content,
                );
                baseline += LINE_H;
            }
        });
    }
}

/// Pre-render the window background: body, title bar, border, separator.
///
/// Fills are axis-aligned rectangles with anti-aliasing disabled and
/// `BlendMode::Source`, which are tiny-skia's fast paths.
fn build_chrome(w: i32, h: i32) -> Option<Pixmap> {
    let mut pm = Pixmap::new(w as u32, h as u32)?;
    let (wf, hf) = (w as f32, h as f32);

    let mut paint = Paint::default();
    paint.anti_alias = false;
    paint.blend_mode = BlendMode::Source;

    // Body.
    paint.set_color_rgba8(18, 20, 32, 255);
    pm.fill_rect(
        Rect::from_xywh(0.0, 0.0, wf, hf)?,
        &paint,
        Transform::identity(),
        None,
    );

    // Title bar.
    paint.set_color_rgba8(44, 50, 80, 255);
    pm.fill_rect(
        Rect::from_xywh(0.0, 0.0, wf, TITLE_H as f32)?,
        &paint,
        Transform::identity(),
        None,
    );

    // Separator under the title bar.
    paint.set_color_rgba8(90, 110, 170, 255);
    pm.fill_rect(
        Rect::from_xywh(0.0, TITLE_H as f32, wf, 1.0)?,
        &paint,
        Transform::identity(),
        None,
    );

    // 1px border.
    let border = PathBuilder::from_rect(Rect::from_xywh(0.5, 0.5, wf - 1.0, hf - 1.0)?);
    let mut border_paint = Paint::default();
    border_paint.anti_alias = true;
    border_paint.set_color_rgba8(120, 150, 220, 255);
    let stroke = Stroke {
        width: 1.0,
        ..Stroke::default()
    };
    pm.stroke_path(&border, &border_paint, &stroke, Transform::identity(), None);

    Some(pm)
}
