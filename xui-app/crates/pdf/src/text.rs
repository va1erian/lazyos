//! The text layer: which characters a page draws, and where.
//!
//! The page is interpreted with a device that draws nothing and records each
//! glyph's Unicode text (from `ToUnicode`, the glyph name or the encoding;
//! hayro's `as_unicode`) and its origin. Find and selection (plan phase P4)
//! build on it; [`PageText::plain`] is enough for tests and copy-all.

use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_interpret::{
    interpret_page, BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image,
    ImageDrawProps, SoftMask,
};
use hayro::kurbo::{Affine, BezPath, Point, Rect};

use crate::Renderer;

/// One drawn glyph, in page points with the origin at the displayed page's
/// top-left corner (after `/Rotate`), y growing downwards.
#[derive(Debug, Clone, PartialEq)]
pub struct TextGlyph {
    pub text: String,
    pub x: f32,
    pub y: f32,
    /// The glyph's em size in points.
    pub size: f32,
    /// How far along the baseline the next glyph starts, in points (an
    /// estimate of 0.5 em when the font does not say).
    pub advance: f32,
}

/// Every glyph a page draws, in content-stream order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PageText {
    pub glyphs: Vec<TextGlyph>,
}

impl PageText {
    /// The page's text with a space where glyphs are apart and a line break
    /// where the baseline moves: content-stream order, no column analysis.
    pub fn plain(&self) -> String {
        let mut out = String::new();
        let mut prev: Option<&TextGlyph> = None;
        for g in &self.glyphs {
            if let Some(p) = prev {
                let size = p.size.max(g.size).max(1.0);
                // TeX and many producers draw no space glyphs: a gap past the
                // previous glyph's advance is a word break.
                let gap = g.x - (p.x + p.advance);
                if (g.y - p.y).abs() > size * 0.5 {
                    out.push('\n');
                } else if (gap > size * 0.15 || g.x < p.x - size * 0.5)
                    && g.text != " "
                    && p.text != " "
                {
                    out.push(' ');
                }
            }
            out.push_str(&g.text);
            prev = Some(g);
        }
        out
    }
}

/// Glyph-space units per em.
const GLYPH_UNITS: f64 = 1000.0;

struct TextDevice {
    glyphs: Vec<TextGlyph>,
}

impl<'a> Device<'a> for TextDevice {
    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        for glyph in run.glyphs() {
            let Some(text) = glyph.as_unicode() else {
                continue;
            };
            let text = match text {
                BfString::Char(c) => c.to_string(),
                BfString::String(s) => s,
            };
            // Glyph space is 1/1000 em, as in PDF font widths.
            let t = props.transform * glyph.transform();
            let origin = t * Point::ZERO;
            let size = (t.determinant().abs().sqrt() * GLYPH_UNITS) as f32;
            let advance = match &**glyph {
                Glyph::Outline(o) => o.advance_width().map(|w| {
                    let end = t * Point::new(w as f64, 0.0);
                    (end - origin).hypot() as f32
                }),
                Glyph::Type3(_) => None,
            };
            self.glyphs.push(TextGlyph {
                text,
                x: origin.x as f32,
                y: origin.y as f32,
                size,
                advance: advance.unwrap_or(size * 0.5),
            });
        }
    }

    fn draw_path(&mut self, _: &BezPath, _: DrawProps<'a>, _: &DrawMode) {}
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_clip_rect(&mut self, _: &Rect) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_image(&mut self, _: Image<'a, '_>, _: ImageDrawProps<'a>) {}
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}

impl<'d> Renderer<'d> {
    /// The text layer of page `index`, or `None` past the end.
    pub fn page_text(&self, index: usize) -> Option<PageText> {
        let page = self.doc.pdf.pages().get(index)?;
        let (w, h) = page.render_dimensions();
        let transform: Affine = page.initial_transform(true).to_kurbo();
        let mut ctx = Context::new(
            transform,
            Rect::new(0.0, 0.0, w as f64, h as f64),
            &self.text_cache,
            page.xref(),
            self.settings.clone(),
        );
        let mut device = TextDevice { glyphs: Vec::new() };
        interpret_page(page, &mut ctx, &mut device);
        Some(PageText {
            glyphs: device.glyphs,
        })
    }
}
