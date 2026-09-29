#![forbid(unsafe_code)]

//! Text shaping and rasterisation, via `cosmic-text`.
//!
//! The shaping state ([`FontSystem`] and [`SwashCache`]) is heavy and not
//! `Send`, so it lives in a thread-local and is shared by every canvas on the
//! UI thread.

use std::cell::RefCell;
use std::sync::Arc;

use cosmic_text::{Align, Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache};
use tiny_skia::{Pixmap, PremultipliedColorU8};

use xui_core::backend::{TextAlign, TextMetrics, TextStyle, TextVAlign};
use xui_core::geometry::Rect;

thread_local! {
    /// Font files registered with [`set_default_font`] / [`add_font`] before the
    /// shaper is first used on this thread.
    static PENDING_FONTS: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
    /// The family every run uses, set with [`set_default_family`]; `None`
    /// leaves the shaper's sans-serif default.
    static FAMILY: RefCell<Option<String>> = const { RefCell::new(None) };
    static TEXT: RefCell<TextSystem> = RefCell::new(TextSystem::new());
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
/// file declares it, e.g. `"JetBrains Mono"`) instead of the default sans-serif.
pub fn set_default_family(family: &str) {
    FAMILY.with(|slot| *slot.borrow_mut() = Some(family.to_owned()));
}

/// The attributes for a run: the default family, if one was set.
fn attrs(family: &Option<String>) -> Attrs<'_> {
    match family {
        Some(name) => Attrs::new().family(Family::Name(name)),
        None => Attrs::new(),
    }
}

struct TextSystem {
    font_system: FontSystem,
    cache: SwashCache,
}

impl TextSystem {
    fn new() -> TextSystem {
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
fn line_height(size_px: f32) -> f32 {
    size_px * 1.25
}

fn align_of(style: &TextStyle) -> Option<Align> {
    match style.align {
        TextAlign::Start => None,
        TextAlign::Center => Some(Align::Center),
        TextAlign::End => Some(Align::Right),
    }
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
    let family = FAMILY.with(|slot| slot.borrow().clone());
    TEXT.with(|text_system| {
        let text_system = &mut *text_system.borrow_mut();
        let mut buffer = Buffer::new(
            &mut text_system.font_system,
            Metrics::new(size, line_height(size)),
        );
        let wrap = style.wrap.then_some(max_width.max(1) as f32);
        buffer.set_size(wrap, None);
        buffer.set_text(text, &attrs(&family), Shaping::Advanced, align_of(style));
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
/// `style` over what is already there.
pub fn draw(pixmap: &mut Pixmap, text: &str, rect: Rect, style: &TextStyle, dpi: u32) {
    if text.is_empty() || rect.is_empty() {
        return;
    }
    let size = style.size.to_px(dpi).value() as f32;
    let (color_r, color_g, color_b) = (style.color.r, style.color.g, style.color.b);
    let (rect_left, rect_top, rect_w, rect_h) = (rect.left, rect.top, rect.width(), rect.height());
    let family = FAMILY.with(|slot| slot.borrow().clone());

    TEXT.with(|text_system| {
        let text_system = &mut *text_system.borrow_mut();
        let metrics = Metrics::new(size, line_height(size));
        let mut buffer = Buffer::new(&mut text_system.font_system, metrics);
        let wrap = style.wrap.then_some(rect_w.max(1) as f32);
        buffer.set_size(wrap, None);
        buffer.set_text(text, &attrs(&family), Shaping::Advanced, align_of(style));
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
                    rect_left + x + align_offset,
                    rect_top + top_offset + y,
                    w,
                    h,
                    [color_r, color_g, color_b],
                    alpha,
                );
            },
        );
    });
}

/// Blends `color` with coverage `alpha` (0..=255) over a `w` x `h` glyph block
/// at `(left, top)`, clipped to the pixmap.
fn blend(pixmap: &mut Pixmap, left: i32, top: i32, w: u32, h: u32, color: [u8; 3], alpha: u32) {
    let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
    let a = alpha as f32 / 255.0;
    let pixels = pixmap.pixels_mut();
    for gy in 0..h as i32 {
        for gx in 0..w as i32 {
            let (x, y) = (left + gx, top + gy);
            if x < 0 || y < 0 || x >= pw || y >= ph {
                continue;
            }
            let at = (y * pw + x) as usize;
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
