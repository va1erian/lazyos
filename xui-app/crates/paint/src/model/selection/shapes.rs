#![forbid(unsafe_code)]

//! Selection shapes rasterised to masks: ellipse, polygon (even-odd) and the
//! runs a magic wand finds.
//!
//! Every shape is sampled at pixel centres, so a shape the user drew from
//! corner to corner covers exactly the pixels whose middle lies inside it.
//! Nothing is allocated outside `clip` (the document), however far a drag
//! strayed.

use super::Selection;
use crate::model::rect::Rect;

/// A polygon edge with a height: it crosses the rows `top..bottom`
/// (half-open on y, so a vertex on a row is counted once).
#[derive(Clone, Copy)]
struct Edge {
    top: i32,
    bottom: i32,
    /// The endpoint the x is measured from, and the slope per row.
    a: (i32, i32),
    b: (i32, i32),
}

impl Edge {
    /// The edge from `a` to `b`, `None` when it is horizontal.
    fn new(a: (i32, i32), b: (i32, i32)) -> Option<Edge> {
        (a.1 != b.1).then(|| Edge {
            top: a.1.min(b.1),
            bottom: a.1.max(b.1),
            a,
            b,
        })
    }

    /// Where the edge crosses the row `y`.
    fn x_at(&self, y: i32) -> f64 {
        let t = f64::from(y - self.a.1) / f64::from(self.b.1 - self.a.1);
        f64::from(self.a.0) + t * f64::from(self.b.0 - self.a.0)
    }
}

/// Coordinates beyond this are clamped before any arithmetic.
const LIMIT: i32 = 1 << 24;

impl Selection {
    /// The ellipse inscribed in `bounds`, restricted to `clip`.
    pub fn ellipse(bounds: Rect, clip: Rect) -> Option<Selection> {
        let area = bounds.intersect(clip);
        if area.is_empty() {
            return None;
        }
        let (w, h) = (i128::from(bounds.width()), i128::from(bounds.height()));
        let width = area.width() as usize;
        let mut mask = vec![0u8; area.area()];
        for y in area.y0..area.y1 {
            // Twice the distance of the row's centre from the ellipse's centre.
            let dy = 2 * i128::from(y - bounds.y0) + 1 - h;
            // x is inside iff dx^2 * h^2 <= w^2 * (h^2 - dy^2), with dx likewise doubled.
            let reach = (w * w * (h * h - dy * dy)) as u128 / (h * h) as u128;
            let m = reach.isqrt() as i128;
            let lo = i128::from(bounds.x0) + (w - 1 - m + 1).div_euclid(2);
            let hi = i128::from(bounds.x0) + (w - 1 + m).div_euclid(2);
            let from = lo.max(i128::from(area.x0)) as i32;
            let to = (hi + 1).min(i128::from(area.x1)) as i32;
            if from < to {
                let row = (y - area.y0) as usize * width;
                mask[row + (from - area.x0) as usize..row + (to - area.x0) as usize].fill(255);
            }
        }
        Selection::from_mask(area, mask)
    }

    /// The polygon through `points`, closed, filled by the even-odd rule (a
    /// pixel is in when a ray from it crosses an odd number of edges), so a
    /// self-crossing lasso leaves holes where it overlaps itself. Each point
    /// is the centre of its pixel. Restricted to `clip`.
    pub fn polygon(points: &[(i32, i32)], clip: Rect) -> Option<Selection> {
        if points.len() < 3 {
            return None;
        }
        let pts: Vec<(i32, i32)> = points
            .iter()
            .map(|(x, y)| (*x.clamp(&-LIMIT, &LIMIT), *y.clamp(&-LIMIT, &LIMIT)))
            .collect();
        let span = pts
            .iter()
            .fold(Rect::EMPTY, |r, p| r.union(Rect::from_points(*p, *p)));
        let area = span.intersect(clip);
        if area.is_empty() {
            return None;
        }
        let width = area.width() as usize;
        let mut mask = vec![0u8; area.area()];
        // An edge table: the edges with a height, top first. A row only looks
        // at the edges that cross it, so the cost is the edges plus the area,
        // not the edges times the rows.
        let mut edges: Vec<Edge> = pts
            .iter()
            .enumerate()
            .filter_map(|(i, a)| Edge::new(*a, pts[(i + 1) % pts.len()]))
            .collect();
        edges.sort_by_key(|e| e.top);
        let (mut next, mut active) = (0, Vec::<Edge>::new());
        let mut crossings: Vec<f64> = Vec::new();
        for y in area.y0..area.y1 {
            while next < edges.len() && edges[next].top <= y {
                active.push(edges[next]);
                next += 1;
            }
            active.retain(|e| y < e.bottom);
            crossings.clear();
            crossings.extend(active.iter().map(|e| e.x_at(y)));
            crossings.sort_by(f64::total_cmp);
            for pair in crossings.as_chunks::<2>().0 {
                // Pixel x is in when pair.0 <= x < pair.1.
                let from = (pair[0].ceil() as i64).max(i64::from(area.x0)) as i32;
                let to = (pair[1].ceil() as i64).min(i64::from(area.x1)) as i32;
                if from < to {
                    let row = (y - area.y0) as usize * width;
                    mask[row + (from - area.x0) as usize..row + (to - area.x0) as usize].fill(255);
                }
            }
        }
        Selection::from_mask(area, mask)
    }

    /// The pixels in `spans` (`(y, x0, x1)`, `x1` exclusive), restricted to
    /// `clip`: what a flood fill or a global colour match found.
    pub fn from_spans(spans: &[(i32, i32, i32)], clip: Rect) -> Option<Selection> {
        let span = spans.iter().fold(Rect::EMPTY, |r, (y, x0, x1)| {
            r.union(Rect {
                x0: *x0,
                y0: *y,
                x1: *x1,
                y1: y.saturating_add(1),
            })
        });
        let area = span.intersect(clip);
        if area.is_empty() {
            return None;
        }
        let width = area.width() as usize;
        let mut mask = vec![0u8; area.area()];
        for (y, x0, x1) in spans {
            if *y < area.y0 || *y >= area.y1 {
                continue;
            }
            let from = (*x0).max(area.x0);
            let to = (*x1).min(area.x1);
            if from < to {
                let row = (*y - area.y0) as usize * width;
                mask[row + (from - area.x0) as usize..row + (to - area.x0) as usize].fill(255);
            }
        }
        Selection::from_mask(area, mask)
    }
}
