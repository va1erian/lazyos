#![forbid(unsafe_code)]

//! Text shaping and rasterisation, via `cosmic-text`.
//!
//! The shaping state ([`FontSystem`] and [`SwashCache`]) is heavy and not
//! `Send`, so it lives in a thread-local and is shared by every canvas on the
//! UI thread.

use std::cell::RefCell;
use std::sync::Arc;

use cosmic_text::{
    Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight,
    Wrap,
};
use tiny_skia::{Mask, Pixmap, PremultipliedColorU8};

use xui_core::backend::{TextAlign, TextMetrics, TextStyle, TextVAlign, TextWeight};
use xui_core::geometry::{Point, Rect};

/// The active clip for a glyph blit: the device-space bounds every glyph is
/// clamped to, and the rounded-clip coverage mask when one is active.
#[derive(Clone, Copy)]
pub(crate) struct GlyphClip<'a> {
    pub(crate) bounds: Option<Rect>,
    pub(crate) mask: Option<&'a Mask>,
}

thread_local! {
    static TEXT: RefCell<TextSystem> = RefCell::new(TextSystem::new());
    /// Font files registered with [`set_default_font`] / [`add_font`] before the
    /// shaper is first used on this thread.
    static PENDING_FONTS: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
    /// The family every run uses when its [`TextStyle`] names none, set with
    /// [`set_default_family`]; `None` leaves the shaper's default.
    static FAMILY: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Registers the font file this thread's shaper loads, alongside whatever
/// `fontdb` finds (nothing, on a platform without a system font store).
///
/// The shaper builds its database once per thread, so this must be called
/// before the first [`measure`] or [`draw`] on the thread; later calls are
/// ignored. The bytes are owned until then, so a caller can hand over a
/// `include_bytes!` slice via `to_vec`.
pub fn set_default_font(data: Vec<u8>) {
    add_font(data);
}

/// Registers a further font file, with the same timing rule as
/// [`set_default_font`]. Pair it with [`set_default_family`] to make an app
/// (the Terminal's monospace face) use a face other than the default.
pub fn add_font(data: Vec<u8>) {
    PENDING_FONTS.with(|fonts| fonts.borrow_mut().push(data));
}

/// Makes every run on this thread use the family named `family` (as the font
/// file declares it, e.g. `"JetBrains Mono"`) when its [`TextStyle`] names
/// none, instead of the shaper's default.
pub fn set_default_family(family: &str) {
    FAMILY.with(|slot| *slot.borrow_mut() = Some(family.to_owned()));
}

pub(crate) struct TextSystem {
    pub(crate) font_system: FontSystem,
    pub(crate) cache: SwashCache,
}

impl TextSystem {
    pub(crate) fn new() -> TextSystem {
        let pending = PENDING_FONTS.with(|fonts| std::mem::take(&mut *fonts.borrow_mut()));
        let font_system = if pending.is_empty() {
            FontSystem::new()
        } else {
            // `new_with_fonts` still scans the system directories first, but
            // the explicit sources are what supply the glyphs where none exist.
            FontSystem::new_with_fonts(pending.into_iter().map(|data| {
                cosmic_text::fontdb::Source::Binary(
                    Arc::new(data) as Arc<dyn AsRef<[u8]> + Send + Sync>
                )
            }))
        };
        TextSystem {
            font_system,
            cache: SwashCache::new(),
        }
    }
}

/// The line height for a font size, matching the widgets' design convention.
pub(crate) fn line_height(size_px: f32) -> f32 {
    size_px * 1.25
}

/// The shaping attributes for a family, weight and slant, so the measured
/// glyph advances are the ones that get painted.
pub(crate) fn attrs_for<'a>(
    family: Option<&'a str>,
    weight: TextWeight,
    italic: bool,
) -> Attrs<'a> {
    let mut attrs = Attrs::new();
    if let Some(family) = family {
        // The generic name asks the font system for its monospace face, so a
        // host need not name a platform font.
        attrs = attrs.family(if family.eq_ignore_ascii_case("monospace") {
            Family::Monospace
        } else {
            Family::Name(family)
        });
    }
    if weight.value() != 400 {
        attrs = attrs.weight(Weight(weight.value()));
    }
    if italic {
        attrs = attrs.style(Style::Italic);
    }
    attrs
}

/// The shaping attributes for `style`: its family (or `fallback`, the thread's
/// [`set_default_family`]), weight and slant, so the measured glyph advances
/// are the ones that get painted.
fn attrs<'a>(style: &'a TextStyle, fallback: Option<&'a str>) -> Attrs<'a> {
    attrs_for(style.family.as_deref().or(fallback), style.weight, style.italic)
}

/// The family set with [`set_default_family`], if any, cloned so a caller can
/// borrow it for the duration of a shaping call.
pub(crate) fn default_family() -> Option<String> {
    FAMILY.with(|slot| slot.borrow().clone())
}

/// Measures `text` for `style` at `dpi`, wrapping to `max_width` when the style
/// asks for it.
pub fn measure(text: &str, style: &TextStyle, dpi: u32, max_width: i32) -> TextMetrics {
    if text.is_empty() {
        let height = line_height(style.size.to_px(dpi).value() as f32).round() as i32;
        return TextMetrics {
            width: 0,
            height,
            ascent: height * 3 / 4,
            descent: height / 4,
        };
    }
    let size = style.size.to_px(dpi).value() as f32;
    let mut height = 0.0f32;
    let mut width = 0.0f32;
    TEXT.with(|text_system| {
        let text_system = &mut *text_system.borrow_mut();
        let mut buffer = Buffer::new(
            &mut text_system.font_system,
            Metrics::new(size, line_height(size)),
        );
        // Size to `max_width` even without wrapping: alignment corrects against
        // the buffer width, while `Wrap::None` keeps a non-wrapping run on one
        // line. `run.line_w` stays the run's own advance either way.
        buffer.set_size(Some(max_width.max(1) as f32), None);
        buffer.set_wrap(if style.wrap {
            Wrap::WordOrGlyph
        } else {
            Wrap::None
        });
        let family = default_family();
        buffer.set_text(
            text,
            &attrs(style, family.as_deref()),
            Shaping::Advanced,
            // Alignment is applied per line at draw time: cosmic-text also
            // aligns a non-wrapped line against the buffer width, which would
            // apply the offset twice.
            None,
        );
        buffer.shape_until_scroll(&mut text_system.font_system, false);
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            height += run.line_height;
        }
    });
    let height = height.round() as i32;
    TextMetrics {
        width: width.round() as i32,
        height,
        ascent: height * 3 / 4,
        descent: height / 4,
    }
}

/// Draws `text` inside `rect` on `pixmap`, blending the glyph coverage of
/// `style` over what is already there and clamping each glyph block to `clip`.
pub fn draw(
    pixmap: &mut Pixmap,
    text: &str,
    rect: Rect,
    style: &TextStyle,
    dpi: u32,
    clip: GlyphClip<'_>,
) {
    if text.is_empty() || rect.is_empty() {
        return;
    }
    let size = style.size.to_px(dpi).value() as f32;
    let (color_r, color_g, color_b) = (style.color.r, style.color.g, style.color.b);
    let (rect_left, rect_top, rect_w, rect_h) = (rect.left, rect.top, rect.width(), rect.height());

    TEXT.with(|text_system| {
        let text_system = &mut *text_system.borrow_mut();
        let metrics = Metrics::new(size, line_height(size));
        let mut buffer = Buffer::new(&mut text_system.font_system, metrics);
        // Size to the target rect even without wrapping so alignment corrects
        // against it; only `style.wrap` lets the run break across lines.
        buffer.set_size(Some(rect_w.max(1) as f32), None);
        buffer.set_wrap(if style.wrap {
            Wrap::WordOrGlyph
        } else {
            Wrap::None
        });
        let family = default_family();
        buffer.set_text(
            text,
            &attrs(style, family.as_deref()),
            Shaping::Advanced,
            // Alignment is applied per line at draw time: cosmic-text also
            // aligns a non-wrapped line against the buffer width, which would
            // apply the offset twice.
            None,
        );
        buffer.shape_until_scroll(&mut text_system.font_system, false);

        let total_height: f32 = buffer.layout_runs().map(|run| run.line_height).sum();
        let top_offset = match style.valign {
            TextVAlign::Top => 0,
            TextVAlign::Middle => ((rect_h as f32 - total_height) / 2.0).max(0.0) as i32,
        };
        // Horizontal alignment relative to the target rectangle's width. The
        // shaper only aligns wrapped paragraphs, so a natural-width run (the
        // common case here) needs the offset applied at draw time. One entry
        // per line: (line top, line width).
        let lines: Vec<(f32, f32)> = buffer
            .layout_runs()
            .map(|run| (run.line_top, run.line_w))
            .collect();

        buffer.draw(
            &mut text_system.font_system,
            &mut text_system.cache,
            Color::rgba(255, 255, 255, 255),
            |x, y, w, h, color| {
                let alpha = (color.0 >> 24) & 0xFF;
                if alpha == 0 {
                    return;
                }
                let line_width = lines
                    .iter()
                    .rev()
                    .find(|(top, _)| *top <= y as f32)
                    .map_or(0.0, |(_, width)| *width);
                let align_offset = match style.align {
                    TextAlign::Start => 0.0,
                    TextAlign::Center => (rect_w as f32 - line_width) / 2.0,
                    TextAlign::End => rect_w as f32 - line_width,
                }
                .max(0.0)
                .round() as i32;
                blend(
                    pixmap,
                    Point::new(rect_left + x + align_offset, rect_top + top_offset + y),
                    w,
                    h,
                    [color_r, color_g, color_b],
                    alpha,
                    clip,
                );
            },
        );
    });
}

/// Blends `color` with coverage `alpha` (0..=255) over a `w` x `h` glyph block
/// at `origin`, clipped to the pixmap and to `clip`.
pub(crate) fn blend(
    pixmap: &mut Pixmap,
    origin: Point,
    w: u32,
    h: u32,
    color: [u8; 3],
    alpha: u32,
    clip: GlyphClip<'_>,
) {
    let (left, top) = (origin.x, origin.y);
    let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
    // A whole block beyond the clip rectangle needs no per-pixel work.
    if let Some(bounds) = clip.bounds
        && (left + w as i32 <= bounds.left
            || top + h as i32 <= bounds.top
            || left >= bounds.right
            || top >= bounds.bottom)
    {
        return;
    }
    let pixels = pixmap.pixels_mut();
    for gy in 0..h as i32 {
        for gx in 0..w as i32 {
            let (x, y) = (left + gx, top + gy);
            if x < 0 || y < 0 || x >= pw || y >= ph {
                continue;
            }
            if let Some(bounds) = clip.bounds
                && (x < bounds.left || y < bounds.top || x >= bounds.right || y >= bounds.bottom)
            {
                continue;
            }
            let at = (y * pw + x) as usize;
            let coverage = clip
                .mask
                .map_or(255, |mask| mask.data().get(at).copied().unwrap_or(0));
            if coverage == 0 {
                continue;
            }
            let a = (alpha * coverage as u32) as f32 / 65025.0;
            let dst = pixels[at];
            let mix = |index: usize, channel: u8| {
                let source = channel as f32 * a;
                let base = match index {
                    0 => dst.red(),
                    1 => dst.green(),
                    _ => dst.blue(),
                };
                (source + base as f32 * (1.0 - a)).round() as u8
            };
            let (r, g, b) = (mix(0, color[0]), mix(1, color[1]), mix(2, color[2]));
            if let Some(pixel) = PremultipliedColorU8::from_rgba(r, g, b, 255) {
                pixels[at] = pixel;
            }
        }
    }
}
