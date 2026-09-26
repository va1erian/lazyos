//! A tiny command interpreter hosted inside a window.
//!
//! Typing edits a prompt line; Enter runs a command. Arrow keys move the
//! window, Page Up/Down scroll the output, Home/End jump. Two commands render
//! demos in the window body: `box` (static) and `ball` (animated).

use crate::arch;
use crate::font;
use crate::gfx::Color;
use crate::gfxlib;
use crate::input::keyboard::Key;
use crate::surface::{RgbaBuffer, Surface};
use crate::text;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use tiny_skia::{BlendMode, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

const TITLE_H: i32 = 26;
const PAD: i32 = 8;
const LINE_H: i32 = font::LINE_HEIGHT;
const TITLE: &str = "LazyOS shell";

const BODY: Color = Color::rgb(16, 18, 30);
const TEXT: Color = Color::rgb(205, 210, 225);
const PROMPT: Color = Color::rgb(130, 230, 150);
const HEADER: Color = Color::rgb(235, 238, 255);
const SCROLL_TRACK: Color = Color::rgb(30, 34, 50);
const SCROLL_THUMB: Color = Color::rgb(120, 150, 220);

/// What the caller should do after a key is handled.
pub enum Effect {
    None,
    Redraw,
    /// Run the animated ball demo, then redraw the CLI.
    Ball,
}

/// The window-hosted command interpreter.
pub struct Cli {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    chrome: Pixmap,
    lines: Vec<String>,
    input: String,
    scroll: usize,
    show_boxes: bool,
    show_scene: bool,
}

impl Cli {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Option<Self> {
        let mut cli = Cli {
            x,
            y,
            w,
            h,
            chrome: build_chrome(w, h)?,
            lines: Vec::new(),
            input: String::new(),
            scroll: 0,
            show_boxes: false,
            show_scene: false,
        };
        for line in [
            "LazyOS shell - type 'help' for commands",
            "try: box, ball, echo <text>, clear, pos",
        ] {
            cli.push_line(line.to_string());
        }
        Some(cli)
    }

    pub fn rect(&self) -> (i32, i32, i32, i32) {
        (self.x, self.y, self.w, self.h)
    }

    pub fn move_by(&mut self, dx: i32, dy: i32, fbw: i32, fbh: i32) {
        self.x = (self.x + dx).clamp(0, (fbw - self.w).max(0));
        self.y = (self.y + dy).clamp(0, (fbh - self.h).max(0));
    }

    /// The content area (inside the chrome, leaving room for the scrollbar).
    pub fn content_rect(&self) -> (i32, i32, i32, i32) {
        (
            self.x + PAD,
            self.y + TITLE_H + PAD,
            self.w - PAD * 2 - 10,
            self.h - TITLE_H - PAD * 2,
        )
    }

    fn visible_lines(&self) -> usize {
        (self.content_rect().3 / LINE_H).max(1) as usize
    }

    pub fn push_line(&mut self, line: String) {
        self.lines.push(line);
    }

    /// Fill the content area with the body colour (used by demos).
    pub fn clear_content(&self, surface: &mut impl Surface) {
        let (cx, cy, cw, ch) = self.content_rect();
        surface.fill_rect(cx, cy, cx + cw, cy + ch, BODY);
    }

    pub fn on_key(&mut self, key: Key) -> Effect {
        if self.show_boxes || self.show_scene {
            // Any key leaves a demo/bench view.
            self.show_boxes = false;
            self.show_scene = false;
            return Effect::Redraw;
        }
        match key {
            Key::Char(c) => {
                self.input.push(c);
                Effect::Redraw
            }
            Key::Space => {
                self.input.push(' ');
                Effect::Redraw
            }
            Key::Tab => {
                self.input.push_str("  ");
                Effect::Redraw
            }
            Key::Backspace => {
                self.input.pop();
                Effect::Redraw
            }
            Key::Escape => {
                self.input.clear();
                Effect::Redraw
            }
            Key::Enter => self.execute(),
            Key::PageUp => {
                self.scroll = (self.scroll + self.page()).min(self.lines.len());
                Effect::Redraw
            }
            Key::PageDown => {
                self.scroll = self.scroll.saturating_sub(self.page());
                Effect::Redraw
            }
            Key::Home => {
                self.scroll = self.lines.len();
                Effect::Redraw
            }
            Key::End => {
                self.scroll = 0;
                Effect::Redraw
            }
            _ => Effect::None,
        }
    }

    fn page(&self) -> usize {
        self.visible_lines().saturating_sub(1).max(1)
    }

    fn execute(&mut self) -> Effect {
        let command = self.input.trim().to_string();
        self.push_line(format!("> {}", self.input));
        self.input.clear();
        self.scroll = 0;

        let mut parts = command.splitn(2, ' ');
        let name = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("").trim();

        match name {
            "" => {}
            "help" => {
                for line in [
                    "commands:",
                    "  help          show this help",
                    "  echo <text>   print text",
                    "  box           draw a colour grid in the window",
                    "  ball          animate bouncing balls (any key to stop)",
                    "  bench         draw the scene with tiny-skia vs our own rasterizer",
                    "  clear         clear the output",
                    "  pos           show the window position",
                    "  quit          (no-op; the shell is the demo)",
                ] {
                    self.push_line(line.to_string());
                }
            }
            "echo" => self.push_line(arg.to_string()),
            "box" => {
                self.push_line("drawing colour grid...".to_string());
                self.show_boxes = true;
            }
            "ball" => {
                self.push_line("running ball demo (any key to stop)".to_string());
                return Effect::Ball;
            }
            "bench" => {
                let (_, _, cw, ch) = self.content_rect();
                let t0 = arch::rdtsc();
                let _ = crate::skia::build_scene(cw as u32, ch as u32);
                let skia_cycles = arch::rdtsc().wrapping_sub(t0);

                let mut scratch = RgbaBuffer::new(cw.max(1) as usize, ch.max(1) as usize);
                let t1 = arch::rdtsc();
                gfxlib::scene(&mut scratch, 0, 0, cw, ch);
                let gfx_cycles = arch::rdtsc().wrapping_sub(t1);

                self.push_line(format!("same scene, {}x{}:", cw, ch));
                self.push_line(format!("  tiny-skia: {} cycles", skia_cycles));
                self.push_line(format!("  gfxlib:    {} cycles", gfx_cycles));
                crate::serial_println!(
                    "bench {}x{}: tiny-skia {} cyc, gfxlib {} cyc",
                    cw,
                    ch,
                    skia_cycles,
                    gfx_cycles
                );
                self.show_scene = true;
            }
            "clear" => self.lines.clear(),
            "pos" => self.push_line(format!("window at ({}, {})", self.x, self.y)),
            other => self.push_line(format!("unknown command: {other}")),
        }
        Effect::Redraw
    }

    /// Draw the whole window (chrome + content + scrollbar) at its position.
    pub fn draw_into(&self, surface: &mut impl Surface) {
        surface.blit_rgba_at(
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
            surface,
            self.x + PAD,
            self.y + TITLE_H - 8,
            TITLE,
            HEADER,
            whole,
        );

        self.draw_scrollbar(surface);

        if self.show_boxes {
            self.draw_boxes(surface);
        } else {
            self.draw_text(surface);
        }
    }

    fn draw_scrollbar(&self, surface: &mut impl Surface) {
        let (_, cy, _, ch) = self.content_rect();
        let total = self.lines.len().max(1);
        let visible = self.visible_lines();
        if total <= visible {
            return;
        }
        let track_x = self.x + self.w - 8;
        let thumb_h = (ch * visible as i32 / total as i32).max(12);
        let thumb_y = cy + (ch - thumb_h) * self.scroll as i32 / (total - visible).max(1) as i32;
        surface.fill_rect(track_x, cy, track_x + 4, cy + ch, SCROLL_TRACK);
        surface.fill_rect(
            track_x,
            thumb_y,
            track_x + 4,
            thumb_y + thumb_h,
            SCROLL_THUMB,
        );
    }

    fn draw_text(&self, surface: &mut impl Surface) {
        let (cx, cy, cw, ch) = self.content_rect();
        if self.show_scene {
            gfxlib::scene(surface, cx, cy, cw, ch);
        } else {
            surface.fill_rect(cx, cy, cx + cw, cy + ch, BODY);
        }

        let clip = text::Rect {
            x0: cx,
            y0: cy,
            x1: cx + cw,
            y1: cy + ch,
        };
        let visible = self.visible_lines();
        let total = self.lines.len() + 1; // +1 for the prompt line
        let from_bottom = self.scroll.min(total.saturating_sub(1));
        let end = total - from_bottom; // exclusive index of last shown line
        let start = end.saturating_sub(visible);

        let mut baseline = cy + font::ASCENDER.max(0);
        for index in start..end {
            if index < self.lines.len() {
                text::draw_text(surface, cx, baseline, &self.lines[index], TEXT, clip);
            } else {
                // Prompt line.
                let prompt = format!("> {}", self.input);
                text::draw_text(surface, cx, baseline, &prompt, PROMPT, clip);
                let cursor_x = cx + text::text_width(&prompt) + 2;
                surface.fill_rect(
                    cursor_x,
                    baseline - font::ASCENDER.max(0),
                    cursor_x + 2,
                    baseline + font::DESCENDER.unsigned_abs() as i32,
                    PROMPT,
                );
            }
            baseline += LINE_H;
        }
    }

    /// Demo 1: a grid of colour-interpolated cells filling the content area.
    fn draw_boxes(&self, surface: &mut impl Surface) {
        let (cx, cy, cw, ch) = self.content_rect();
        surface.fill_rect(cx, cy, cx + cw, cy + ch, BODY);
        let cols = 10;
        let rows = 7;
        let cw_cell = cw / cols;
        let ch_cell = ch / rows;
        for row in 0..rows {
            for col in 0..cols {
                let t = (row * cols + col) as f32 / (cols * rows - 1) as f32;
                let color = Color::rgb(
                    (40.0 + 215.0 * t) as u8,
                    (200.0 * (1.0 - t)) as u8,
                    (80.0 + 175.0 * (1.0 - t)) as u8,
                );
                let x0 = cx + col * cw_cell + 2;
                let y0 = cy + row * ch_cell + 2;
                surface.fill_rect(x0, y0, x0 + cw_cell - 4, y0 + ch_cell - 4, color);
            }
        }
    }
}

/// Pre-render the window chrome (body, title bar, border). Uses tiny-skia's
/// fast paths: axis-aligned fills, anti-aliasing off, `BlendMode::Source`.
fn build_chrome(w: i32, h: i32) -> Option<Pixmap> {
    let mut pm = Pixmap::new(w as u32, h as u32)?;
    let (wf, hf) = (w as f32, h as f32);

    let mut paint = Paint::default();
    paint.anti_alias = false;
    paint.blend_mode = BlendMode::Source;

    paint.set_color_rgba8(16, 18, 30, 255);
    pm.fill_rect(
        Rect::from_xywh(0.0, 0.0, wf, hf)?,
        &paint,
        Transform::identity(),
        None,
    );

    paint.set_color_rgba8(44, 50, 80, 255);
    pm.fill_rect(
        Rect::from_xywh(0.0, 0.0, wf, TITLE_H as f32)?,
        &paint,
        Transform::identity(),
        None,
    );

    paint.set_color_rgba8(90, 110, 170, 255);
    pm.fill_rect(
        Rect::from_xywh(0.0, TITLE_H as f32, wf, 1.0)?,
        &paint,
        Transform::identity(),
        None,
    );

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
