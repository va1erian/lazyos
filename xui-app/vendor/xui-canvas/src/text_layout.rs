#![forbid(unsafe_code)]

//! The portable [`TextShaper`]/[`TextLayout`] adapter over `cosmic-text`.
//!
//! A [`CosmicShaper`] owns one [`TextSystem`] behind an `Arc`/`Mutex`, so the
//! shaper is `Send + Sync` and a worker thread can shape text; the
//! [`CosmicLayout`] it returns carries the same system, so a glyph is resolved
//! against the font database it was shaped with even when it is drawn later.

use std::any::Any;
use std::sync::{Arc, Mutex, PoisonError};

use cosmic_text::{Buffer, Color, Cursor, Metrics, Shaping};
use tiny_skia::Pixmap;
use xui_core::backend::{FontSpec, Rgba, TextHit, TextLayout, TextShaper};
use xui_core::geometry::{Point, Rect};

use crate::text::{GlyphClip, TextSystem, attrs_for, blend, default_family, line_height};

/// A `Send + Sync` cosmic-text shaper. Cloning shares the font system.
#[derive(Clone)]
pub(crate) struct CosmicShaper {
    system: Arc<Mutex<TextSystem>>,
}

impl CosmicShaper {
    /// Creates the shaper and its (lazily loaded) font system.
    pub(crate) fn new() -> CosmicShaper {
        CosmicShaper {
            system: Arc::new(Mutex::new(TextSystem::new())),
        }
    }

    /// Shapes `text`, the shared work behind [`TextShaper::layout`].
    fn shape(&self, text: &str, spec: &FontSpec, max_width: f32, dpi: u32) -> Buffer {
        let size = spec.size.to_px(dpi).value().max(1) as f32;
        let mut system = self.system.lock().unwrap_or_else(PoisonError::into_inner);
        let mut buffer = Buffer::new(
            &mut system.font_system,
            Metrics::new(size, line_height(size)),
        );
        let wrap = max_width.is_finite().then_some(max_width.max(1.0));
        buffer.set_size(wrap, None);
        // Fall back to the thread's default family (LazyOS installs its bundled
        // face there) when the spec names none.
        let fallback = default_family();
        buffer.set_text(
            text,
            &attrs_for(
                spec.family.as_deref().or(fallback.as_deref()),
                spec.weight,
                spec.italic,
            ),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut system.font_system, false);
        buffer
    }
}

impl TextShaper for CosmicShaper {
    fn layout(&self, text: &str, spec: &FontSpec, max_width: f32, dpi: u32) -> Box<dyn TextLayout> {
        Box::new(CosmicLayout {
            buffer: Mutex::new(self.shape(text, spec, max_width, dpi)),
            system: Arc::clone(&self.system),
        })
    }
}

/// A shaped `cosmic-text` buffer, locked because drawing needs `&mut Buffer`.
pub(crate) struct CosmicLayout {
    buffer: Mutex<Buffer>,
    system: Arc<Mutex<TextSystem>>,
}

/// The widest line and the height of all lines of `buffer`.
fn size(buffer: &Buffer) -> (f32, f32) {
    let mut width = 0.0f32;
    let mut height = 0.0f32;
    for run in buffer.layout_runs() {
        width = width.max(run.line_w);
        height += run.line_height;
    }
    (width, height)
}

/// The byte offset of the line's first character in the whole shaped text.
fn line_start(buffer: &Buffer, line: usize) -> usize {
    buffer.lines[..line]
        .iter()
        .map(|line| line.text().len() + line.ending().as_str().len())
        .sum()
}

impl TextLayout for CosmicLayout {
    fn width(&self) -> f32 {
        let buffer = self.buffer.lock().unwrap_or_else(PoisonError::into_inner);
        size(&buffer).0
    }

    fn height(&self) -> f32 {
        let buffer = self.buffer.lock().unwrap_or_else(PoisonError::into_inner);
        size(&buffer).1
    }

    fn hit_test_point(&self, x: f32, y: f32) -> TextHit {
        let buffer = self.buffer.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(cursor) = buffer.hit(x, y) else {
            return TextHit {
                byte_index: 0,
                inside: false,
            };
        };
        let (width, height) = size(&buffer);
        TextHit {
            byte_index: line_start(&buffer, cursor.line) + cursor.index,
            inside: x >= 0.0 && y >= 0.0 && x <= width && y <= height,
        }
    }

    fn selection_rects(&self, byte_start: usize, byte_end: usize) -> Vec<Rect> {
        if byte_end <= byte_start {
            return Vec::new();
        }
        let buffer = self.buffer.lock().unwrap_or_else(PoisonError::into_inner);
        let mut rects = Vec::new();
        let mut base = 0;
        for (line, entry) in buffer.lines.iter().enumerate() {
            let length = entry.text().len();
            let from = byte_start.max(base);
            let to = byte_end.min(base + length);
            if to > from {
                let cursor = |index| Cursor::new(line, index);
                for run in buffer.layout_runs().filter(|run| run.line_i == line) {
                    for (x, width) in run.highlight(cursor(from - base), cursor(to - base)) {
                        let top = run.line_top.round() as i32;
                        rects.push(Rect::new(
                            x.round() as i32,
                            top,
                            (x + width).round() as i32,
                            (run.line_top + run.line_height).round() as i32,
                        ));
                    }
                }
            }
            base += length + entry.ending().as_str().len();
        }
        rects
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Draws `layout` on `pixmap` with its top-left corner at `origin`, in `color`,
/// clamping each glyph block to `clip`.
pub(crate) fn draw_layout(
    pixmap: &mut Pixmap,
    layout: &CosmicLayout,
    origin: Point,
    color: Rgba,
    clip: GlyphClip<'_>,
) {
    let mut buffer = layout.buffer.lock().unwrap_or_else(PoisonError::into_inner);
    if buffer.layout_runs().next().is_none() {
        return;
    }
    let mut system = layout.system.lock().unwrap_or_else(PoisonError::into_inner);
    let TextSystem { font_system, cache } = &mut *system;
    buffer.draw(
        font_system,
        cache,
        Color::rgba(255, 255, 255, 255),
        |x, y, w, h, glyph| {
            let alpha = (glyph.0 >> 24) & 0xFF;
            if alpha == 0 {
                return;
            }
            blend(
                pixmap,
                Point::new(origin.x + x, origin.y + y),
                w,
                h,
                [color.r, color.g, color.b],
                alpha,
                clip,
            );
        },
    );
}
