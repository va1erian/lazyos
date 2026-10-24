//! Polygon coverage: 4x4 subsamples per pixel, non-zero winding, and an
//! outline that is "within `r` of an edge, outside the body".
//!
//! Units: vertices are in 1/64 pixel (`du * px / 8` from design units).

use alloc::vec::Vec;

/// Vertex units per pixel.
pub(crate) const SUB: u32 = 64;
/// Subsamples per pixel side.
const SS: i64 = 4;

/// One polygon set drawn in one colour, with an outline outside it.
pub(crate) struct Layer {
    /// Polygons in design units (1/16 of a grid cell, 512 per side).
    pub polys: Vec<Vec<(i32, i32)>>,
    /// The fill colour, `0xRRGGBB`.
    pub fill: u32,
    /// The outline colour, `0xRRGGBB`.
    pub outline: u32,
    /// A thinner outline (the spinner's dots).
    pub thin: bool,
}

/// The outline radius in vertex units: 1/16 of the size, at least a pixel.
pub(crate) fn outline_radius(px: u32) -> u32 {
    (px * SUB / 32).max(SUB)
}

type Pt = (i64, i64);

struct Edge {
    a: Pt,
    b: Pt,
    dx: i64,
    dy: i64,
    len2: i64,
    min_x: i64,
    max_x: i64,
    min_y: i64,
    max_y: i64,
}

struct PreparedPoly {
    edges: Vec<Edge>,
    max_x: i64,
    min_y: i64,
    max_y: i64,
}

struct PreparedLayer {
    polys: Vec<PreparedPoly>,
    all_edges: Vec<Edge>,
    bbox_min_x: i64,
    bbox_max_x: i64,
    bbox_min_y: i64,
    bbox_max_y: i64,
}

fn prepare_layer(polys: &[Vec<(i32, i32)>], px: u32) -> PreparedLayer {
    let mut all_edges = Vec::new();
    let mut bbox_min_x = i64::MAX;
    let mut bbox_max_x = i64::MIN;
    let mut bbox_min_y = i64::MAX;
    let mut bbox_max_y = i64::MIN;

    let prepared_polys = polys
        .iter()
        .map(|poly| {
            let pts: Vec<Pt> = poly
                .iter()
                .map(|&(x, y)| (x as i64 * px as i64 / 8, y as i64 * px as i64 / 8))
                .collect();
            let mut p_min_x = i64::MAX;
            let mut p_max_x = i64::MIN;
            let mut p_min_y = i64::MAX;
            let mut p_max_y = i64::MIN;

            let edges: Vec<Edge> = (0..pts.len())
                .map(|i| {
                    let a = pts[i];
                    let b = pts[(i + 1) % pts.len()];
                    let dx = b.0 - a.0;
                    let dy = b.1 - a.1;
                    let len2 = dx * dx + dy * dy;
                    let min_x = a.0.min(b.0);
                    let max_x = a.0.max(b.0);
                    let min_y = a.1.min(b.1);
                    let max_y = a.1.max(b.1);

                    Edge {
                        a,
                        b,
                        dx,
                        dy,
                        len2,
                        min_x,
                        max_x,
                        min_y,
                        max_y,
                    }
                })
                .collect();

            for e in &edges {
                p_min_x = p_min_x.min(e.min_x);
                p_max_x = p_max_x.max(e.max_x);
                p_min_y = p_min_y.min(e.min_y);
                p_max_y = p_max_y.max(e.max_y);

                bbox_min_x = bbox_min_x.min(e.min_x);
                bbox_max_x = bbox_max_x.max(e.max_x);
                bbox_min_y = bbox_min_y.min(e.min_y);
                bbox_max_y = bbox_max_y.max(e.max_y);

                all_edges.push(Edge {
                    a: e.a,
                    b: e.b,
                    dx: e.dx,
                    dy: e.dy,
                    len2: e.len2,
                    min_x: e.min_x,
                    max_x: e.max_x,
                    min_y: e.min_y,
                    max_y: e.max_y,
                });
            }

            PreparedPoly {
                edges,
                max_x: p_max_x,
                min_y: p_min_y,
                max_y: p_max_y,
            }
        })
        .collect();
    PreparedLayer {
        polys: prepared_polys,
        all_edges,
        bbox_min_x,
        bbox_max_x,
        bbox_min_y,
        bbox_max_y,
    }
}

/// Non-zero winding of `p` against every polygon (edges implicitly closed).
fn inside(prep: &PreparedLayer, p: Pt) -> bool {
    prep.polys.iter().any(|poly| {
        if p.1 < poly.min_y || p.1 > poly.max_y || p.0 > poly.max_x {
            return false;
        }
        let mut winding = 0i32;
        for e in &poly.edges {
            if e.min_y <= p.1 && e.max_y >= p.1 {
                let side = e.dx * (p.1 - e.a.1) - (p.0 - e.a.0) * e.dy;
                if e.a.1 <= p.1 {
                    if e.b.1 > p.1 && side > 0 {
                        winding += 1;
                    }
                } else if e.b.1 <= p.1 && side < 0 {
                    winding -= 1;
                }
            }
        }
        winding != 0
    })
}

/// Squared distance from `p` to segment in `Edge`.
fn seg_dist2(p: Pt, e: &Edge) -> i64 {
    let (mut qx, mut qy) = (e.a.0, e.a.1);
    if e.len2 > 0 {
        let t = ((p.0 - e.a.0) * e.dx + (p.1 - e.a.1) * e.dy).clamp(0, e.len2);
        qx = e.a.0 + (e.dx * t + e.len2 / 2) / e.len2;
        qy = e.a.1 + (e.dy * t + e.len2 / 2) / e.len2;
    }
    (p.0 - qx).pow(2) + (p.1 - qy).pow(2)
}

fn edge_dist2_bounded(prep: &PreparedLayer, p: Pt, limit: i64) -> i64 {
    let mut best = limit;
    for e in &prep.all_edges {
        let dx = if p.0 < e.min_x {
            e.min_x - p.0
        } else if p.0 > e.max_x {
            p.0 - e.max_x
        } else {
            0
        };
        let dy = if p.1 < e.min_y {
            e.min_y - p.1
        } else if p.1 > e.max_y {
            p.1 - e.max_y
        } else {
            0
        };
        if dx * dx + dy * dy >= best {
            continue;
        }

        let d = seg_dist2(p, e);
        if d < best {
            best = d;
            if best == 0 {
                return 0;
            }
        }
    }
    best
}

fn edge_dist2(prep: &PreparedLayer, p: Pt) -> i64 {
    edge_dist2_bounded(prep, p, i64::MAX)
}

/// Composite `layer` over `pixels` (a `side x side` premultiplied ARGB sprite
/// whose design square starts `margin` pixels in).
pub(crate) fn draw_layer(pixels: &mut [u32], side: u32, margin: u32, px: u32, layer: &Layer) {
    let prep = prepare_layer(&layer.polys, px);
    if prep.all_edges.is_empty() {
        return;
    }
    let radius = if layer.thin {
        SUB as i64 * 3 / 4
    } else {
        outline_radius(px) as i64
    };
    let r2 = radius * radius;
    let near = (radius + SUB as i64).pow(2);
    let full = (SUB as i64).pow(2);
    let sub = SUB as i64;

    let margin_sub = margin as i64 * sub;
    let near_dist = radius + sub;
    // Bounding box in units to bound pixel grid iteration.
    let min_i = ((((prep.bbox_min_x - near_dist) / sub) + margin as i64) - 1).clamp(0, side as i64);
    let max_i = ((((prep.bbox_max_x + near_dist) / sub) + margin as i64) + 2).clamp(0, side as i64);
    let min_j = ((((prep.bbox_min_y - near_dist) / sub) + margin as i64) - 1).clamp(0, side as i64);
    let max_j = ((((prep.bbox_max_y + near_dist) / sub) + margin as i64) + 2).clamp(0, side as i64);

    for j in min_j..max_j {
        for i in min_i..max_i {
            let origin = (i * sub - margin_sub, j * sub - margin_sub);
            let centre = (origin.0 + sub / 2, origin.1 + sub / 2);
            let d2 = edge_dist2(&prep, centre);
            let is_in = inside(&prep, centre);
            let (fill, dil) = if is_in && d2 > full {
                (16, 16)
            } else if !is_in && d2 > near {
                continue;
            } else {
                subsample(&prep, origin, r2)
            };
            if dil == 0 {
                continue;
            }
            let alpha = (dil * 255 / 16) as u32;
            let src = alpha << 24
                | mix_channel(fill, dil, layer, 16) << 16
                | mix_channel(fill, dil, layer, 8) << 8
                | mix_channel(fill, dil, layer, 0);
            let dst = &mut pixels[(j * side as i64 + i) as usize];
            *dst = over(src, *dst);
        }
    }
}

/// One premultiplied channel of a pixel with `fill` body samples and `dil -
/// fill` outline samples out of 16.
fn mix_channel(fill: i64, dil: i64, layer: &Layer, shift: u32) -> u32 {
    let body = i64::from((layer.fill >> shift) & 0xff);
    let edge = i64::from((layer.outline >> shift) & 0xff);
    ((fill * body + (dil - fill) * edge) / 16) as u32
}

/// `(fill, dilated)` sample counts out of 16 for the pixel at `origin`.
fn subsample(prep: &PreparedLayer, origin: Pt, r2: i64) -> (i64, i64) {
    let (mut fill, mut dil) = (0, 0);
    for b in 0..SS {
        for a in 0..SS {
            let p = (
                origin.0 + (2 * a + 1) * SUB as i64 / (2 * SS),
                origin.1 + (2 * b + 1) * SUB as i64 / (2 * SS),
            );
            if inside(prep, p) {
                fill += 1;
                dil += 1;
            } else if edge_dist2_bounded(prep, p, r2 + 1) <= r2 {
                dil += 1;
            }
        }
    }
    (fill, dil)
}

/// Premultiplied source-over.
fn over(src: u32, dst: u32) -> u32 {
    let inv = 255 - (src >> 24);
    let mix = |shift: u32| {
        let s = (src >> shift) & 0xff;
        let d = (dst >> shift) & 0xff;
        (s + (d * inv + 127) / 255).min(255) << shift
    };
    mix(24) | mix(16) | mix(8) | mix(0)
}
