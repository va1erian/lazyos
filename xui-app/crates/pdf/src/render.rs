//! Drawing a rectangle of a page into RGBA pixels.

use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_interpret::{InterpreterCache, InterpreterSettings};
use hayro::kurbo::Affine;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::{PixmapMut, RasterizerSettings, RenderContext, Resources, TargetInit};
use hayro::{RenderCache, RenderSettings};

use crate::Document;

/// The largest tile side, in pixels: hayro's render context is `u16`-sized.
pub const MAX_TILE_SIDE: u32 = u16::MAX as u32;

/// Opaque RGBA8 pixels (the page is drawn over white), row-major.
#[derive(Clone, PartialEq, Eq)]
pub struct Tile {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl core::fmt::Debug for Tile {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Tile({}x{})", self.width, self.height)
    }
}

/// Renders pages of one [`Document`], keeping hayro's font and glyph caches
/// between calls. It borrows the document and is not `Send`: each worker
/// thread makes its own from a shared `Arc<Document>`.
pub struct Renderer<'d> {
    pub(crate) doc: &'d Document,
    cache: RenderCache<'d>,
    /// The text layer's own font cache (`RenderCache` keeps its one private).
    pub(crate) text_cache: InterpreterCache<'d>,
    pub(crate) settings: InterpreterSettings,
}

impl<'d> Renderer<'d> {
    pub fn new(doc: &'d Document) -> Self {
        Self {
            doc,
            cache: RenderCache::new(),
            text_cache: InterpreterCache::new(),
            settings: InterpreterSettings::default(),
        }
    }

    /// The pixel size of page `index` at `scale` pixels per point.
    pub fn page_pixels(&self, index: usize, scale: f32) -> Option<(u32, u32)> {
        let size = self.doc.page_size(index)?;
        Some((
            (size.width * scale).round().max(1.0) as u32,
            (size.height * scale).round().max(1.0) as u32,
        ))
    }

    /// Draws the `width` x `height` pixel rectangle at (`x`, `y`) of page
    /// `index` rendered at `scale` pixels per point. Pixels outside the page
    /// stay white. `None` for a page that does not exist or an empty or
    /// oversized rectangle.
    pub fn render_tile(
        &self,
        index: usize,
        scale: f32,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    ) -> Option<Tile> {
        if width == 0 || height == 0 || width > MAX_TILE_SIDE || height > MAX_TILE_SIDE {
            return None;
        }
        if !(scale.is_finite() && scale > 0.0) {
            return None;
        }
        let page = self.doc.pdf.pages().get(index)?;
        let (w16, h16) = (width as u16, height as u16);
        let transform = Affine::translate((-(x as f64), -(y as f64)))
            * Affine::scale(scale as f64)
            * page.initial_transform(true).to_kurbo();

        let mut ctx = RenderContext::new(w16, h16);
        hayro::render_into(
            page,
            &self.cache,
            &self.settings,
            &RenderSettings::default(),
            &mut ctx,
            transform,
        );
        ctx.flush();

        let mut rgba = vec![0u8; width as usize * height as usize * 4];
        let target = PixmapMut::new(w16, h16, &mut rgba)?;
        ctx.render_with(
            target,
            &mut Resources::default(),
            RasterizerSettings {
                target_init: TargetInit::Clear(WHITE),
                ..Default::default()
            },
        );
        Some(Tile {
            width,
            height,
            rgba,
        })
    }

    /// The whole page at `scale`, as one tile.
    pub fn render_page(&self, index: usize, scale: f32) -> Option<Tile> {
        let (w, h) = self.page_pixels(index, scale)?;
        self.render_tile(index, scale, 0, 0, w, h)
    }
}
