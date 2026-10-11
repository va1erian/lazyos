//! Polygon coverage: 4x4 subsamples per pixel, non-zero winding, and an
//! outline that is "within `r` of an edge, outside the body".
//!
//! Units: vertices are in 1/64 pixel (`du * px / 8` from design units).

use alloc::vec::Vec;

/// Vertex units per pixel.
pub(crate) const SUB: u32 = 64;
/// Subsamples per pixel side.
const SS: i64 = 4;

/// One polygon set drawn in one grey, with a dark outline outside it.
pub(crate) struct Layer {
    /// Polygons in design units (1/16 of a grid cell, 512 per side).
    pub polys: Vec<Vec<(i32, i32)>>,
    /// The fill grey (0..=255); the outline is always black.
    pub grey: u32,
    /// A thinner outline (the spinner's dots).
    pub thin: bool,
}

/// The outline radius in vertex units: 1/16 of the size, at least a pixel.
pub(crate) fn outline_radius(px: u32) -> u32 {
    (px * SUB / 32).max(SUB)
}

type Pt = (i64, i64);

fn to_units(polys: &[Vec<(i32, i32)>], px: u32) -> Vec<Vec<Pt>> {
    polys
        .iter()
        .map(|poly| {
            poly.iter()
                .map(|&(x, y)| (x as i64 * px as i64 / 8, y as i64 * px as i64 / 8))
                .collect()
        })
        .collect()
}

/// Non-zero winding of `p` against every polygon (edges implicitly closed).
fn inside(polys: &[Vec<Pt>], p: Pt) -> bool {
    polys.iter().any(|poly| {
        let mut winding = 0i32;
        for i in 0..poly.len() {
            let a = poly[i];
            let b = poly[(i + 1) % poly.len()];
            let side = (b.0 - a.0) * (p.1 - a.1) - (p.0 - a.0) * (b.1 - a.1);
            if a.1 <= p.1 {
                if b.1 > p.1 && side > 0 {
                    winding += 1;
                }
            } else if b.1 <= p.1 && side < 0 {
                winding -= 1;
            }
        }
        winding != 0
    })
}

/// Squared distance from `p` to the segment `a..b`.
fn seg_dist2(p: Pt, a: Pt, b: Pt) -> i64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let (mut qx, mut qy) = (a.0, a.1);
    if len2 > 0 {
        let t = ((p.0 - a.0) * dx + (p.1 - a.1) * dy).clamp(0, len2);
        qx = a.0 + (dx * t + len2 / 2) / len2;
        qy = a.1 + (dy * t + len2 / 2) / len2;
    }
    (p.0 - qx).pow(2) + (p.1 - qy).pow(2)
}

fn edge_dist2(polys: &[Vec<Pt>], p: Pt) -> i64 {
    let mut best = i64::MAX;
    for poly in polys {
        for i in 0..poly.len() {
            best = best.min(seg_dist2(p, poly[i], poly[(i + 1) % poly.len()]));
        }
    }
    best
}

/// Composite `layer` over `pixels` (a `side x side` premultiplied ARGB sprite
/// whose design square starts `margin` pixels in).
pub(crate) fn draw_layer(pixels: &mut [u32], side: u32, margin: u32, px: u32, layer: &Layer) {
    let polys = to_units(&layer.polys, px);
    let radius = if layer.thin {
        SUB as i64 * 3 / 4
    } else {
        outline_radius(px) as i64
    };
    let r2 = radius * radius;
    let near = (radius + SUB as i64).pow(2);
    let full = (SUB as i64).pow(2);
    let sub = SUB as i64;
    for j in 0..side as i64 {
        for i in 0..side as i64 {
            let origin = ((i - margin as i64) * sub, (j - margin as i64) * sub);
            let centre = (origin.0 + sub / 2, origin.1 + sub / 2);
            let d2 = edge_dist2(&polys, centre);
            let is_in = inside(&polys, centre);
            let (fill, dil) = if is_in && d2 > full {
                (16, 16)
            } else if !is_in && d2 > near {
                continue;
            } else {
                subsample(&polys, origin, r2)
            };
            if dil == 0 {
                continue;
            }
            let alpha = (dil * 255 / 16) as u32;
            let rgb = (fill * layer.grey as i64 / 16) as u32;
            let src = alpha << 24 | rgb << 16 | rgb << 8 | rgb;
            let dst = &mut pixels[(j * side as i64 + i) as usize];
            *dst = over(src, *dst);
        }
    }
}

/// `(fill, dilated)` sample counts out of 16 for the pixel at `origin`.
fn subsample(polys: &[Vec<Pt>], origin: Pt, r2: i64) -> (i64, i64) {
    let (mut fill, mut dil) = (0, 0);
    for b in 0..SS {
        for a in 0..SS {
            let p = (
                origin.0 + (2 * a + 1) * SUB as i64 / (2 * SS),
                origin.1 + (2 * b + 1) * SUB as i64 / (2 * SS),
            );
            if inside(polys, p) {
                fill += 1;
                dil += 1;
            } else if edge_dist2(polys, p) <= r2 {
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
