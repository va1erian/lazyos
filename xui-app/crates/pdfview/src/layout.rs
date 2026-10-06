//! Where pages sit: the document laid out as one column of pages at a zoom,
//! in device pixels, and which tiles of which pages a viewport shows. Pure
//! arithmetic, so it is tested without a window.

use std::ops::Range;

use lazypdf::PageSize;

/// The side of a rendered tile, in device pixels.
pub const TILE: i32 = 256;
/// Space around and between pages at 96 dpi; scaled with the window's DPI.
const GAP_96: i32 = 12;

/// A rectangle in document pixels (the column of pages, origin top-left).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PxRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl PxRect {
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    fn intersects(&self, other: &PxRect) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// How the zoom is chosen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zoom {
    /// The current page fills the viewport's width.
    FitWidth,
    /// The current page fits whole in the viewport.
    FitPage,
    /// A fixed factor: 1.0 shows a page at its printed size.
    Factor(f32),
}

/// The fixed zoom steps Zoom in/out walk through.
pub const STEPS: [f32; 17] = [
    0.25, 0.33, 0.5, 0.67, 0.75, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0, 6.0, 8.0,
];

/// The next step above (`up`) or below the factor `now`.
pub fn step(now: f32, up: bool) -> f32 {
    let eps = 0.005;
    if up {
        STEPS
            .iter()
            .copied()
            .find(|s| *s > now + eps)
            .unwrap_or(STEPS[STEPS.len() - 1])
    } else {
        STEPS
            .iter()
            .rev()
            .copied()
            .find(|s| *s < now - eps)
            .unwrap_or(STEPS[0])
    }
}

/// The pages at one scale, stacked top to bottom and centred horizontally.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// Device pixels per PDF point.
    pub scale: f32,
    pub rects: Vec<PxRect>,
    /// The whole column's size, margins included.
    pub width: i32,
    pub height: i32,
}

impl Layout {
    /// Lays `pages` out at `scale` pixels per point for a viewport
    /// `view_width` pixels wide; `dpi` sizes the gaps.
    pub fn new(pages: &[PageSize], scale: f32, view_width: i32, dpi: u32) -> Layout {
        let gap = gap(dpi);
        let sizes: Vec<(i32, i32)> = pages.iter().map(|p| page_pixels(*p, scale)).collect();
        let widest = sizes.iter().map(|s| s.0).max().unwrap_or(0);
        let width = (widest + 2 * gap).max(view_width);
        let mut y = gap;
        let rects = sizes
            .iter()
            .map(|&(w, h)| {
                let rect = PxRect {
                    x: (width - w) / 2,
                    y,
                    w,
                    h,
                };
                y += h + gap;
                rect
            })
            .collect();
        Layout {
            scale,
            rects,
            width,
            height: y,
        }
    }

    /// The pages any part of which lies in `top..top + height`.
    pub fn visible(&self, top: i32, height: i32) -> Range<usize> {
        let bottom = top + height.max(0);
        let first = self.rects.partition_point(|r| r.bottom() <= top);
        let end = self.rects.partition_point(|r| r.y < bottom);
        first..end.max(first)
    }

    /// The page shown at document row `y` (the nearest one in a gap).
    pub fn page_at(&self, y: i32) -> usize {
        let index = self.rects.partition_point(|r| r.bottom() <= y);
        index.min(self.rects.len().saturating_sub(1))
    }

    /// The tiles (column, row) of page `index` that `view` overlaps, nearest
    /// to the view's centre first.
    pub fn tiles(&self, index: usize, view: PxRect) -> Vec<(u32, u32)> {
        let Some(page) = self.rects.get(index) else {
            return Vec::new();
        };
        if !page.intersects(&view) {
            return Vec::new();
        }
        let left = (view.x - page.x).max(0) / TILE;
        let top = (view.y - page.y).max(0) / TILE;
        let right = ((view.right() - page.x).min(page.w) - 1) / TILE;
        let bottom = ((view.bottom() - page.y).min(page.h) - 1) / TILE;
        let centre = (view.x + view.w / 2, view.y + view.h / 2);
        let mut tiles: Vec<(u32, u32)> = (top..=bottom)
            .flat_map(|row| (left..=right).map(move |col| (col as u32, row as u32)))
            .collect();
        tiles.sort_by_key(|&(col, row)| {
            let cx = page.x + col as i32 * TILE + TILE / 2 - centre.0;
            let cy = page.y + row as i32 * TILE + TILE / 2 - centre.1;
            i64::from(cx) * i64::from(cx) + i64::from(cy) * i64::from(cy)
        });
        tiles
    }

    /// The pixel rectangle tile (`col`, `row`) of page `index` covers, in the
    /// page's own pixels.
    pub fn tile_rect(&self, index: usize, col: u32, row: u32) -> Option<PxRect> {
        let page = self.rects.get(index)?;
        let x = col as i32 * TILE;
        let y = row as i32 * TILE;
        if x >= page.w || y >= page.h {
            return None;
        }
        Some(PxRect {
            x,
            y,
            w: TILE.min(page.w - x),
            h: TILE.min(page.h - y),
        })
    }
}

/// A page's size in device pixels at `scale`: the rounding `lazypdf` uses.
pub fn page_pixels(page: PageSize, scale: f32) -> (i32, i32) {
    (
        (page.width * scale).round().max(1.0) as i32,
        (page.height * scale).round().max(1.0) as i32,
    )
}

/// The gap between pages at `dpi`.
pub fn gap(dpi: u32) -> i32 {
    (GAP_96 * dpi.max(1) as i32 + 48) / 96
}

/// Device pixels per point for `zoom` in a `view` (width, height) viewport,
/// with `current` the page Fit page fits.
pub fn scale_for(
    zoom: Zoom,
    pages: &[PageSize],
    current: usize,
    view: (i32, i32),
    dpi: u32,
) -> f32 {
    let natural = dpi.max(1) as f32 / 72.0;
    let room = |px: i32| (px - 2 * gap(dpi)).max(16) as f32;
    let page = pages
        .get(current)
        .filter(|p| p.width > 0.0 && p.height > 0.0);
    let scale = match (zoom, page) {
        (Zoom::Factor(f), _) => f * natural,
        (Zoom::FitWidth, Some(p)) => room(view.0) / p.width,
        (Zoom::FitPage, Some(p)) => (room(view.0) / p.width).min(room(view.1) / p.height),
        (_, None) => natural,
    };
    // Within the zoom steps' range, so a tiny window or a sliver of a page
    // never asks for a degenerate or enormous render.
    scale.clamp(STEPS[0] * natural, STEPS[STEPS.len() - 1] * natural)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a4() -> PageSize {
        PageSize {
            width: 595.0,
            height: 842.0,
        }
    }

    #[test]
    fn pages_stack_with_gaps_and_centre() {
        let pages = [
            a4(),
            PageSize {
                width: 842.0,
                height: 595.0,
            },
        ];
        let l = Layout::new(&pages, 1.0, 1000, 96);
        assert_eq!(
            l.rects[0],
            PxRect {
                x: (1000 - 595) / 2,
                y: 12,
                w: 595,
                h: 842
            }
        );
        assert_eq!(l.rects[1].y, 12 + 842 + 12);
        assert_eq!(l.rects[1].x, (1000 - 842) / 2);
        assert_eq!(l.height, 12 + 842 + 12 + 595 + 12);
        // A narrow viewport scrolls sideways: the column is the widest page.
        let narrow = Layout::new(&pages, 1.0, 300, 96);
        assert_eq!(narrow.width, 842 + 24);
        assert_eq!(narrow.rects[1].x, 12);
    }

    #[test]
    fn visible_pages_and_the_page_at_a_row() {
        let pages = vec![a4(); 10];
        let l = Layout::new(&pages, 0.5, 400, 96);
        let h = l.rects[0].h; // 421
        assert_eq!(l.visible(0, 100), 0..1);
        assert_eq!(
            l.visible(l.rects[1].y - 5, 10),
            1..2,
            "a gap and the page below"
        );
        assert_eq!(l.visible(0, h * 3), 0..3);
        assert_eq!(l.visible(l.height + 100, 100), 10..10);
        assert_eq!(l.page_at(0), 0);
        assert_eq!(l.page_at(l.rects[4].y + 3), 4);
        assert_eq!(l.page_at(i32::MAX), 9);
    }

    #[test]
    fn tiles_cover_the_view_nearest_first() {
        let pages = [a4()];
        let l = Layout::new(&pages, 1.0, 595 + 24, 96);
        let page = l.rects[0];
        // The view shows the whole page: every tile, 3 x 4.
        let all = l.tiles(
            0,
            PxRect {
                x: 0,
                y: 0,
                w: 2000,
                h: 2000,
            },
        );
        assert_eq!(all.len(), 3 * 4);
        // A small view in the middle of tile (1, 1) asks for it first.
        let mid = PxRect {
            x: page.x + 300,
            y: page.y + 300,
            w: 100,
            h: 100,
        };
        assert_eq!(l.tiles(0, mid), vec![(1, 1)]);
        // The last tiles are clipped to the page.
        assert_eq!(
            l.tile_rect(0, 2, 3),
            Some(PxRect {
                x: 512,
                y: 768,
                w: 595 - 512,
                h: 842 - 768
            })
        );
        assert_eq!(l.tile_rect(0, 3, 0), None);
        assert!(l
            .tiles(
                0,
                PxRect {
                    x: 0,
                    y: page.bottom() + 1,
                    w: 100,
                    h: 100
                }
            )
            .is_empty());
    }

    #[test]
    fn fit_modes_and_steps() {
        let pages = [
            a4(),
            PageSize {
                width: 842.0,
                height: 595.0,
            },
        ];
        let view = (842 + 24, 600);
        let fw = scale_for(Zoom::FitWidth, &pages, 1, view, 96);
        assert!((fw - 1.0).abs() < 1e-3, "the landscape page: {fw}");
        let fw = scale_for(Zoom::FitWidth, &pages, 0, view, 96);
        assert!((fw - 842.0 / 595.0).abs() < 1e-3, "the portrait page: {fw}");
        let fp = scale_for(Zoom::FitPage, &pages, 0, view, 96);
        assert!((fp - (600.0 - 24.0) / 842.0).abs() < 1e-3, "{fp}");
        let hundred = scale_for(Zoom::Factor(1.0), &pages, 0, view, 192);
        assert!(
            (hundred - 192.0 / 72.0).abs() < 1e-3,
            "HiDPI doubles the pixels: {hundred}"
        );
        assert_eq!(step(1.0, true), 1.1);
        assert_eq!(step(1.0, false), 0.9);
        assert_eq!(step(1.17, true), 1.25);
        assert_eq!(step(8.0, true), 8.0);
        assert_eq!(step(0.25, false), 0.25);
        let tiny = scale_for(Zoom::FitWidth, &pages, 0, (1, 1), 96);
        assert!(tiny >= 0.25 * 96.0 / 72.0 - 1e-4);
    }
}
