#![forbid(unsafe_code)]

//! A page of a [`Printout`] as PWG Raster, painted in bands.
//!
//! The printer takes portrait sheets only, so the raster is always the
//! portrait sheet. A portrait page is painted 256 rows at a time. A landscape
//! page is turned a quarter turn anticlockwise (its top along the sheet's
//! left edge, as IPP's `landscape` orientation lays it): each band of
//! portrait rows is a strip of the landscape page's columns, painted on its
//! own and read out sideways, so no whole sheet is ever held in memory.

use raster::{ColorSpace, Header, PageEncoder};
use xui_canvas::Surface;
use xui_core::geometry::Rect;
use xui_rich_text::Printout;

/// The printer's resolution: the DeskJet 3700 takes PWG Raster at 300 dpi
/// only, and every IPP Everywhere printer supports it.
pub const DPI: u32 = 300;
/// Rows painted at once: 2.5 MB of RGBA for an A4 band at 300 dpi.
const BAND: u32 = 256;

/// What a page is printed as.
#[derive(Clone, Debug)]
pub struct Format {
    pub color: ColorSpace,
    /// PWG media name, e.g. `iso_a4_210x297mm`.
    pub media: String,
    /// `print-quality`, 0 for the printer's default.
    pub quality: u32,
    /// Pages in the job.
    pub total_pages: u32,
}

/// The portrait sheet's size in pixels, `(width, height)`.
pub fn sheet(out: &Printout) -> (u32, u32) {
    let (w, h) = out.sheet_size();
    if out.is_landscape() { (h, w) } else { (w, h) }
}

/// Encodes sheet `page` of `out` (header and rows), handing the bytes to
/// `sink` band by band.
pub fn render_page(out: &Printout, page: usize, format: &Format, sink: &mut dyn FnMut(Vec<u8>)) {
    let (width, height) = sheet(out);
    let mut encoder = PageEncoder::new(Header {
        width,
        height,
        dpi: out.dpi(),
        color: format.color,
        media: format.media.clone(),
        quality: format.quality,
        total_pages: format.total_pages,
    });
    let mut row = vec![0u8; width as usize * 4];
    let mut top = 0;
    while top < height {
        let bottom = (top + BAND).min(height);
        if out.is_landscape() {
            landscape_band(out, page, (top, bottom), &mut row, &mut encoder);
        } else {
            portrait_band(out, page, (top, bottom), &mut encoder);
        }
        sink(encoder.take_output());
        top = bottom;
    }
    sink(encoder.finish().expect("every row of the sheet was given"));
}

fn portrait_band(
    out: &Printout,
    page: usize,
    (top, bottom): (u32, u32),
    encoder: &mut PageEncoder,
) {
    let width = sheet(out).0;
    let rows = bottom - top;
    let mut surface = Surface::new(width, rows);
    let area = Rect::new(0, top as i32, width as i32, bottom as i32);
    surface.with_canvas_at(
        Rect::new(0, 0, width as i32, rows as i32),
        out.dpi(),
        |canvas| {
            out.paint(canvas, page, area);
        },
    );
    let stride = width as usize * 4;
    for line in surface.pixels().chunks_exact(stride) {
        encoder.push_rgba(line).expect("a band row is a sheet row");
    }
}

/// Portrait rows `top..bottom` of a landscape page turned anticlockwise:
/// portrait pixel `(x, y)` shows landscape pixel `(W - 1 - y, x)`, `W` being
/// the landscape width (the portrait height).
fn landscape_band(
    out: &Printout,
    page: usize,
    (top, bottom): (u32, u32),
    row: &mut [u8],
    encoder: &mut PageEncoder,
) {
    let (portrait_w, portrait_h) = sheet(out);
    let strip = bottom - top;
    let mut surface = Surface::new(strip, portrait_w);
    // Landscape columns W - bottom .. W - top, every landscape row.
    let left = (portrait_h - bottom) as i32;
    let area = Rect::new(left, 0, left + strip as i32, portrait_w as i32);
    surface.with_canvas_at(
        Rect::new(0, 0, strip as i32, portrait_w as i32),
        out.dpi(),
        |canvas| {
            out.paint(canvas, page, area);
        },
    );
    let pixels = surface.pixels();
    for y in top..bottom {
        // The strip column holding landscape column W - 1 - y.
        let column = (bottom - 1 - y) as usize;
        for (x, px) in row.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let at = (x * strip as usize + column) * 4;
            px.copy_from_slice(&pixels[at..at + 4]);
        }
        encoder.push_rgba(row).expect("a turned row is a sheet row");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xui_core::backend::Backend;
    use xui_rich_text::Document;
    use xui_rich_text::model::PageSetup;

    fn printout(text: &str, page: PageSetup, dpi: u32) -> Printout {
        let doc = Document::from_plain_text(text).with_page(page).unwrap();
        let shaper = xui_canvas::OffscreenBackend::new().text_shaper();
        Printout::new(&doc, shaper.as_ref(), dpi)
    }

    fn format(color: ColorSpace) -> Format {
        Format {
            color,
            media: "iso_a4_210x297mm".into(),
            quality: 4,
            total_pages: 1,
        }
    }

    fn decode(out: &Printout, page: usize, color: ColorSpace) -> raster::Page {
        let mut stream = raster::SYNC.to_vec();
        render_page(out, page, &format(color), &mut |bytes| stream.extend(bytes));
        let mut pages = raster::decode(&stream, usize::MAX).unwrap();
        assert_eq!(pages.len(), 1);
        pages.remove(0)
    }

    /// The bounding box of the pixels darker than mid grey in a grey page.
    fn ink(page: &raster::Page) -> Option<(u32, u32, u32, u32)> {
        let w = page.header.width as usize;
        let mut b: Option<(u32, u32, u32, u32)> = None;
        for (i, &v) in page.pixels.iter().enumerate() {
            if v < 128 {
                let (x, y) = ((i % w) as u32, (i / w) as u32);
                b = Some(match b {
                    None => (x, y, x, y),
                    Some((l, t, r, bt)) => (l.min(x), t.min(y), r.max(x), bt.max(y)),
                });
            }
        }
        b
    }

    #[test]
    fn a4_at_300_dpi_is_the_sheet_the_printer_expects() {
        let out = printout("Hello", PageSetup::a4(), DPI);
        let page = decode(&out, 0, ColorSpace::Srgb8);
        assert_eq!(
            (page.header.width, page.header.height, page.header.dpi),
            (2480, 3508, 300)
        );
        assert_eq!(page.pixels.len(), 2480 * 3508 * 3);
    }

    #[test]
    fn portrait_text_starts_at_the_top_left_margin() {
        // A small page at 96 dpi keeps the test fast; the geometry is the same.
        let out = printout("Hello", PageSetup::a4(), 96);
        let page = decode(&out, 0, ColorSpace::Sgray8);
        let (left, top, _, bottom) = ink(&page).expect("the text is printed");
        // A4's normal margins are 25 mm: 94.5 px at 96 dpi.
        assert!((94..110).contains(&left), "left {left}");
        assert!((94..120).contains(&top), "top {top}");
        assert!(bottom < 140);
    }

    #[test]
    fn a_landscape_page_is_turned_onto_a_portrait_sheet() {
        let portrait = printout("Hello", PageSetup::a4(), 96);
        let turned = printout("Hello", PageSetup::a4().rotated(), 96);
        let page = decode(&turned, 0, ColorSpace::Sgray8);
        assert_eq!(
            (page.header.width, page.header.height),
            sheet(&portrait),
            "a portrait sheet"
        );
        // The landscape page's top-left text corner lands at the sheet's
        // bottom-left: the text's top along the left edge, reading upwards.
        let (left, _, _, bottom) = ink(&page).expect("the text is printed");
        let height = page.header.height;
        assert!((94..120).contains(&left), "left {left}");
        assert!(
            (94..110).contains(&(height - 1 - bottom)),
            "bottom margin {}",
            height - 1 - bottom
        );
        // The same text, turned back, matches the landscape page's own paint.
        let w = page.header.width as usize;
        let (lw, lh) = turned.sheet_size();
        let mut surface = Surface::new(lw, lh);
        surface.with_canvas_at(Rect::new(0, 0, lw as i32, lh as i32), 96, |c| {
            turned.paint(c, 0, Rect::new(0, 0, lw as i32, lh as i32))
        });
        let land = surface.pixels();
        for (y, x) in [(100usize, 100usize), (110, 120), (105, 130), (300, 50)] {
            // Landscape (lx, ly) is at portrait (ly, W - 1 - lx).
            let (lx, ly) = (x, y);
            let p = page.pixels[(lw as usize - 1 - lx) * w + ly];
            let l = &land[(ly * lw as usize + lx) * 4..][..3];
            let grey = ((54 * l[0] as u32 + 183 * l[1] as u32 + 19 * l[2] as u32 + 128) >> 8) as u8;
            assert_eq!(p, grey, "landscape ({lx}, {ly})");
        }
    }

    #[test]
    fn every_page_of_a_long_document_renders() {
        let text = "line\n".repeat(120);
        let out = printout(&text, PageSetup::a4(), 96);
        assert!(out.page_count() >= 2);
        for page in 0..out.page_count() {
            assert!(
                ink(&decode(&out, page, ColorSpace::Sgray8)).is_some(),
                "page {page}"
            );
        }
    }
}
