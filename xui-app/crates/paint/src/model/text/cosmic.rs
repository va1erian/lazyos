#![forbid(unsafe_code)]

//! The real [`TextRasterizer`]: `cosmic-text` shaping and `swash` glyph
//! rendering, the stack the desktop's software backend draws UI text with, so the Text tool's
//! pixels match what the rest of the desktop shows.
//!
//! The host registers font bytes ([`CosmicRasterizer::register_font`]); no
//! system font directory is scanned, so the result is the same on the host
//! and in the guest.

use std::cell::RefCell;

use cosmic_text::{
    Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight, Wrap,
};

use crate::model::rect::Rect;
use crate::model::tools::Coverage;

use super::style::{LineGeometry, TextAlign, TextCoverage, TextRasterizer, TextStyle};

/// Line spacing as a multiple of the font size, as the widgets use.
const LINE_HEIGHT: f32 = 1.25;
/// Sizes outside this range are clamped (a hostile size cannot allocate a
/// gigabyte mask).
const SIZE_RANGE: (f32, f32) = (1.0, 1024.0);
/// Blank columns kept right of the ink so slanted glyphs are not clipped.
const PAD: i32 = 2;

struct Shaper {
    fonts: FontSystem,
    cache: SwashCache,
}

/// Lays out and rasterizes text from registered fonts.
pub struct CosmicRasterizer {
    inner: RefCell<Shaper>,
}

impl Default for CosmicRasterizer {
    fn default() -> Self {
        CosmicRasterizer::new()
    }
}

impl CosmicRasterizer {
    /// A rasterizer with no fonts; register some before laying text out.
    pub fn new() -> CosmicRasterizer {
        let db = cosmic_text::fontdb::Database::new();
        CosmicRasterizer {
            inner: RefCell::new(Shaper {
                fonts: FontSystem::new_with_locale_and_db("en-US".to_owned(), db),
                cache: SwashCache::new(),
            }),
        }
    }

    /// Adds the font file `bytes` (TTF/OTF). Unparseable bytes are skipped.
    /// The first family registered also stands in for the generic sans, serif
    /// and monospace names, so a style naming an unknown family still draws.
    pub fn register_font(&mut self, bytes: Vec<u8>) {
        let shaper = self.inner.get_mut();
        let first = shaper.fonts.db().is_empty();
        shaper.fonts.db_mut().load_font_data(bytes);
        if first {
            let name = shaper
                .fonts
                .db()
                .faces()
                .next()
                .and_then(|face| face.families.first())
                .map(|(name, _)| name.clone());
            if let Some(name) = name {
                let db = shaper.fonts.db_mut();
                db.set_sans_serif_family(&name);
                db.set_serif_family(&name);
                db.set_monospace_family(&name);
            }
        }
    }

    /// The registered family names, sorted and without duplicates, for the
    /// options bar's font list.
    pub fn families(&self) -> Vec<String> {
        let shaper = self.inner.borrow();
        let mut names: Vec<String> = shaper
            .fonts
            .db()
            .faces()
            .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// The empty layout: one empty line.
fn blank(height: f32) -> TextCoverage {
    TextCoverage {
        extent: Rect::EMPTY,
        mask: Coverage::new(Rect::EMPTY),
        lines: vec![LineGeometry {
            top: 0.0,
            height,
            baseline: height * 0.8,
            start: 0,
            carets: vec![0.0],
        }],
    }
}

impl TextRasterizer for CosmicRasterizer {
    fn families(&self) -> Vec<String> {
        CosmicRasterizer::families(self)
    }

    fn layout_in(&self, text: &str, style: &TextStyle, window: Rect) -> TextCoverage {
        let size = if style.size.is_finite() {
            style.size.clamp(SIZE_RANGE.0, SIZE_RANGE.1)
        } else {
            SIZE_RANGE.0
        };
        let line_height = size * LINE_HEIGHT;
        let mut guard = self.inner.borrow_mut();
        let Shaper { fonts, cache } = &mut *guard;
        // Shaping through an empty database panics inside cosmic-text.
        if fonts.db().is_empty() {
            return blank(line_height);
        }
        let known = fonts.db().faces().any(|face| {
            face.families
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case(&style.family))
        });
        let mut attrs = Attrs::new().family(if known {
            Family::Name(&style.family)
        } else {
            Family::SansSerif
        });
        if style.bold {
            attrs = attrs.weight(Weight::BOLD);
        }
        if style.italic {
            attrs = attrs.style(Style::Italic);
        }
        let mut buffer = Buffer::new(fonts, Metrics::new(size, line_height));
        buffer.set_wrap(Wrap::None);
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(fonts, false);

        let mut lines = line_table(text, &buffer, line_height);
        let block = lines.iter().map(|l| l.width).fold(0.0_f32, f32::max);
        for line in &mut lines {
            let offset = match style.align {
                TextAlign::Left => 0.0,
                TextAlign::Center => (block - line.width) / 2.0,
                TextAlign::Right => block - line.width,
            };
            line.offset = offset.round();
            for x in &mut line.geometry.carets {
                *x += line.offset;
            }
        }

        let width = (block.ceil() as i32).saturating_add(PAD);
        let height = (lines.len() as f32 * line_height).ceil() as i32;
        let extent = Rect::new(0, 0, width as u32, height as u32);
        // Only the part inside the window is allocated and drawn: the block
        // can be far larger than the layer it is painted on.
        let keep = extent.intersect(window);
        let mut mask = Coverage::new(keep);
        if !keep.is_empty() {
            let stride = keep.width() as usize;
            let data = mask.data_mut();
            let offsets: Vec<(f32, f32)> =
                lines.iter().map(|l| (l.geometry.top, l.offset)).collect();
            buffer.draw(
                fonts,
                cache,
                Color::rgba(255, 255, 255, 255),
                |x, y, w, h, color| {
                    let alpha = color.a();
                    if alpha == 0 {
                        return;
                    }
                    let offset = offsets
                        .iter()
                        .rfind(|(top, _)| *top <= y as f32)
                        .map_or(0.0, |(_, off)| *off) as i32;
                    let glyph = Rect::new(x.saturating_add(offset), y, w, h).intersect(keep);
                    for yy in glyph.y0..glyph.y1 {
                        let row = (yy - keep.y0) as usize * stride;
                        for xx in glyph.x0..glyph.x1 {
                            let slot = &mut data[row + (xx - keep.x0) as usize];
                            *slot = (*slot).max(alpha);
                        }
                    }
                },
            );
            if style.underline {
                for line in lines.iter().filter(|l| l.width > 0.0) {
                    let thickness = (size / 14.0).round().max(1.0) as i32;
                    let top = (line.geometry.baseline + size * 0.1).round() as i32;
                    let (x0, x1) = (line.offset as i32, (line.offset + line.width).ceil() as i32);
                    let bar = Rect {
                        x0,
                        y0: top,
                        x1,
                        y1: top.saturating_add(thickness),
                    }
                    .intersect(keep);
                    for yy in bar.y0..bar.y1 {
                        let row = (yy - keep.y0) as usize * stride;
                        data[row + (bar.x0 - keep.x0) as usize..row + (bar.x1 - keep.x0) as usize]
                            .fill(255);
                    }
                }
            }
        }
        if !style.antialias {
            mask.threshold();
        }
        TextCoverage {
            extent,
            mask,
            lines: lines.into_iter().map(|l| l.geometry).collect(),
        }
    }
}

/// One line while it is being measured.
struct Measured {
    geometry: LineGeometry,
    width: f32,
    offset: f32,
}

/// One entry per `\n`-separated line of `text`, from the shaped runs; lines the
/// shaper produced no run for (empty ones) get a synthetic box.
fn line_table(text: &str, buffer: &Buffer, line_height: f32) -> Vec<Measured> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (i, line) in text.split('\n').enumerate() {
        let count = line.chars().count();
        lines.push(Measured {
            geometry: LineGeometry {
                top: i as f32 * line_height,
                height: line_height,
                baseline: i as f32 * line_height + line_height * 0.8,
                start,
                carets: vec![0.0; count + 1],
            },
            width: 0.0,
            offset: 0.0,
        });
        start += count + 1;
    }
    for run in buffer.layout_runs() {
        let Some(entry) = lines.get_mut(run.line_i) else {
            continue;
        };
        entry.geometry.top = run.line_top;
        entry.geometry.height = run.line_height;
        entry.geometry.baseline = run.line_y;
        entry.width = run.line_w;
        // Byte offset in the line -> character column.
        let columns: Vec<usize> = run.text.char_indices().map(|(b, _)| b).collect();
        let column_of = |byte: usize| columns.partition_point(|&b| b < byte);
        let carets = &mut entry.geometry.carets;
        let n = carets.len();
        carets.fill(f32::NAN);
        carets[n - 1] = run.line_w;
        for glyph in run.glyphs {
            let (first, last) = (column_of(glyph.start), column_of(glyph.end));
            let chars = (last.saturating_sub(first)).max(1);
            for k in 0..chars {
                if let Some(slot) = carets.get_mut(first + k) {
                    *slot = glyph.x + glyph.w * k as f32 / chars as f32;
                }
            }
        }
        // A column no glyph covers (a collapsed cluster) sits where the
        // previous one does.
        let mut previous = 0.0;
        for x in carets.iter_mut() {
            if x.is_nan() {
                *x = previous;
            }
            previous = *x;
        }
    }
    lines
}
