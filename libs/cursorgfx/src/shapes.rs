//! The cursor shapes as polygons on the 32x32 design grid.
//!
//! Coordinates in the tables are half grid cells; [`poly`] turns them into
//! design units (1/16 cell, 512 per side), the unit the rasteriser takes.

use alloc::vec;
use alloc::vec::Vec;

use crate::raster::Layer;
use crate::{Shape, BUSY_FRAMES};

type Poly = Vec<(i32, i32)>;

const LIGHT: u32 = 240;

/// A grey as `0xRRGGBB`.
fn grey(level: u32) -> u32 {
    level << 16 | level << 8 | level
}

/// Half-cell points to a design-unit polygon.
fn poly(half_cells: &[(i32, i32)]) -> Poly {
    half_cells.iter().map(|&(x, y)| (x * 8, y * 8)).collect()
}

fn rect(x0: i32, y0: i32, x1: i32, y1: i32) -> Poly {
    poly(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)])
}

/// The old 8x8 bitmap's silhouette: a right triangle with a short tail,
/// tip at the origin (each bitmap pixel was 4 grid cells).
const ARROW: [(i32, i32); 6] = [(0, 0), (48, 48), (48, 56), (16, 56), (16, 64), (0, 64)];

const HAND: [(i32, i32); 23] = [
    (22, 8),
    (25, 4),
    (29, 4),
    (32, 8),
    (32, 22),
    (34, 20),
    (38, 20),
    (41, 23),
    (42, 24),
    (46, 24),
    (49, 26),
    (50, 28),
    (54, 28),
    (57, 31),
    (57, 44),
    (50, 56),
    (28, 56),
    (20, 44),
    (12, 33),
    (10, 28),
    (14, 26),
    (19, 31),
    (22, 34),
];

/// A double-headed arrow along the x axis.
const DOUBLE: [(i32, i32); 10] = [
    (4, 32),
    (18, 18),
    (18, 27),
    (46, 27),
    (46, 18),
    (60, 32),
    (46, 46),
    (46, 37),
    (18, 37),
    (18, 46),
];

/// cos/sin of 45 degrees in 1/1024.
const R45: i32 = 724;
/// 1.25 in 1/1024: a rotated arrow's tips would otherwise look short.
const GROW: i32 = 1280;

/// `points` rotated by `(c, s)` (1/1024) and scaled by `GROW` about the centre.
fn rotated(points: &[(i32, i32)], c: i32, s: i32) -> Poly {
    let centre = 256;
    points
        .iter()
        .map(|&(x, y)| {
            let (dx, dy) = (x * 8 - centre, y * 8 - centre);
            let rx = (dx * c - dy * s) / 1024;
            let ry = (dx * s + dy * c) / 1024;
            (centre + rx * GROW / 1024, centre + ry * GROW / 1024)
        })
        .collect()
}

fn solid(polys: Vec<Poly>) -> Vec<Layer> {
    vec![Layer {
        polys,
        fill: grey(LIGHT),
        outline: 0,
        thin: false,
    }]
}

/// The arrow scaled by 3/4, leaving the lower right free for the spinner.
fn small_arrow() -> Poly {
    poly(&ARROW)
        .iter()
        .map(|&(x, y)| (x * 3 / 4, y * 3 / 4))
        .collect()
}

const COS: [i32; 8] = [1024, 724, 0, -724, -1024, -724, 0, 724];
const SIN: [i32; 8] = [0, 724, 1024, 724, 0, -724, -1024, -724];

/// A ring of dots at the lower right; the dot at `frame` is the brightest and
/// the ones behind it fade.
fn spinner(frame: u32) -> Vec<Layer> {
    const CENTRE: i32 = 25 * 16;
    const RING: i32 = 5 * 16;
    const DOT: i32 = 29;
    (0..BUSY_FRAMES as usize)
        .map(|k| {
            let age = (k as u32 + BUSY_FRAMES - frame) % BUSY_FRAMES;
            let (cx, cy) = (CENTRE + RING * COS[k] / 1024, CENTRE + RING * SIN[k] / 1024);
            let dot = (0..8)
                .map(|v| (cx + DOT * COS[v] / 1024, cy + DOT * SIN[v] / 1024))
                .collect();
            Layer {
                polys: vec![dot],
                fill: grey(LIGHT - age * 24),
                outline: 0,
                thin: true,
            }
        })
        .collect()
}

/// The layers of `shape`, bottom first.
pub(crate) fn layers(shape: Shape, frame: u32) -> Vec<Layer> {
    match shape {
        Shape::Default => solid(vec![poly(&ARROW)]),
        Shape::Busy => {
            let mut all = solid(vec![small_arrow()]);
            all.extend(spinner(frame));
            all
        }
        Shape::Text => solid(vec![
            rect(20, 6, 44, 12),
            rect(29, 12, 35, 52),
            rect(20, 52, 44, 58),
        ]),
        Shape::Hand => solid(vec![poly(&HAND)]),
        Shape::ResizeH => solid(vec![poly(&DOUBLE)]),
        Shape::ResizeV => {
            let flipped: Vec<(i32, i32)> = DOUBLE.iter().map(|&(x, y)| (y, x)).collect();
            solid(vec![poly(&flipped)])
        }
        Shape::ResizeNwse => solid(vec![rotated(&DOUBLE, R45, R45)]),
        Shape::ResizeNesw => solid(vec![rotated(&DOUBLE, R45, -R45)]),
    }
}

/// The pointer position in design units: the arrow's tip, the hand's
/// fingertip, the centre of everything else.
pub(crate) fn hotspot(shape: Shape) -> (i32, i32) {
    match shape {
        Shape::Default | Shape::Busy => (0, 0),
        Shape::Hand => (14 * 16, 2 * 16),
        _ => (256, 256),
    }
}
