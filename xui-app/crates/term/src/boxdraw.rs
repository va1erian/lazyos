//! Box-drawing (U+2500-257F) and block elements (U+2580-259F) as geometry,
//! so lines join across cells at every size and in every font. The painter
//! fills the rectangles; this only says where.
//!
//! Light and heavy lines are single strokes (heavy is twice as thick);
//! double lines are two light strokes a stroke apart. Dashed forms are drawn
//! solid, rounded corners square, and the diagonals (U+2571-2573) are left
//! to the font.

use crate::boxarms::arms_of;

/// An axis-aligned rectangle in pixels, `x0..x1` by `y0..y1`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Box2 {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl Box2 {
    pub const fn new(x0: i32, y0: i32, x1: i32, y1: i32) -> Box2 {
        Box2 { x0, y0, x1, y1 }
    }

    pub fn width(&self) -> i32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> i32 {
        self.y1 - self.y0
    }
}

/// How much of the foreground a rectangle is painted with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fill {
    Solid,
    /// This many quarters (1-3) of the way from the background to the
    /// foreground: the shade blocks.
    Quarters(u8),
}

/// Whether `c` is drawn as geometry.
pub fn is_geometric(c: char) -> bool {
    matches!(c, '\u{2500}'..='\u{2570}' | '\u{2574}'..='\u{259f}')
}

/// The pixels a stroke of `thickness` centred on `center` covers.
fn span(center: i32, thickness: i32) -> (i32, i32) {
    let lo = center - thickness / 2;
    (lo, lo + thickness)
}

/// The rectangles that draw `c` in `cell`, with strokes `light` pixels thick,
/// or `None` when `c` is not drawn as geometry.
pub fn geometry(c: char, cell: Box2, light: i32) -> Option<Vec<(Box2, Fill)>> {
    if let Some(blocks) = block_element(c, cell) {
        // A one-pixel-wide cell has no room for half of it.
        return Some(
            blocks
                .into_iter()
                .filter(|(b, _)| b.width() > 0 && b.height() > 0)
                .collect(),
        );
    }
    let a = arms_of(c)?;
    let light = light.max(1);
    let thick = |weight: u8| match weight {
        2 => 2 * light,
        _ => light,
    };
    let extent = |weight: u8| match weight {
        0 => 0,
        3 => 3 * light,
        w => thick(w),
    };
    let (cx, cy) = (cell.x0 + cell.width() / 2, cell.y0 + cell.height() / 2);
    let hext = extent(a.left).max(extent(a.right));
    let vext = extent(a.up).max(extent(a.down));
    let (hlo, hhi) = span(cy, hext);
    let (vlo, vhi) = span(cx, vext);
    // The strokes of a double line, one light stroke either side of centre.
    let (up_l, dn_l) = (span(cy - light, light), span(cy + light, light));
    let (lf_l, rt_l) = (span(cx - light, light), span(cx + light, light));
    let mut out = Vec::new();
    // Every rectangle is clipped to the cell: at tiny sizes a double line is
    // wider than the cell, and must not paint a neighbour.
    let mut put = |x0: i32, y0: i32, x1: i32, y1: i32| {
        let (x0, y0) = (x0.max(cell.x0), y0.max(cell.y0));
        let (x1, y1) = (x1.min(cell.x1), y1.min(cell.y1));
        if x1 > x0 && y1 > y0 {
            out.push((Box2::new(x0, y0, x1, y1), Fill::Solid));
        }
    };
    // Vertical arms: each stroke ends where the horizontal arms say.
    for (arm, upward) in [(a.up, true), (a.down, false)] {
        for (side, other, offset) in strokes(arm, a.left, a.right) {
            let (x0, x1) = span(cx + offset * light, thick(arm));
            if upward {
                let end = if side == 3 {
                    up_l.1
                } else if other == 3 {
                    dn_l.1
                } else if hext > 0 {
                    hhi
                } else {
                    cy
                };
                put(x0, cell.y0, x1, end);
            } else {
                let start = if side == 3 {
                    dn_l.0
                } else if other == 3 {
                    up_l.0
                } else if hext > 0 {
                    hlo
                } else {
                    cy
                };
                put(x0, start, x1, cell.y1);
            }
        }
    }
    // Horizontal arms, the same with the axes swapped.
    for (arm, leftward) in [(a.left, true), (a.right, false)] {
        for (side, other, offset) in strokes(arm, a.up, a.down) {
            let (y0, y1) = span(cy + offset * light, thick(arm));
            if leftward {
                let end = if side == 3 {
                    lf_l.1
                } else if other == 3 {
                    rt_l.1
                } else if vext > 0 {
                    vhi
                } else {
                    cx
                };
                put(cell.x0, y0, end, y1);
            } else {
                let start = if side == 3 {
                    rt_l.0
                } else if other == 3 {
                    lf_l.0
                } else if vext > 0 {
                    vlo
                } else {
                    cx
                };
                put(start, y0, cell.x1, y1);
            }
        }
    }
    drop(put);
    Some(out)
}

/// The strokes of an arm of weight `arm`: for each, the weight of the
/// perpendicular arm on its side, the one on the other side, and its offset
/// (in light strokes) from the line's centre. A double arm has two.
fn strokes(arm: u8, negative: u8, positive: u8) -> Vec<(u8, u8, i32)> {
    match arm {
        0 => Vec::new(),
        3 => vec![(negative, positive, -1), (positive, negative, 1)],
        _ => vec![(0, 0, 0)],
    }
}

/// The block elements U+2580-259F.
fn block_element(c: char, cell: Box2) -> Option<Vec<(Box2, Fill)>> {
    let code = c as u32;
    if !(0x2580..=0x259f).contains(&code) {
        return None;
    }
    let (w, h) = (cell.width(), cell.height());
    let solid = |x0, y0, x1, y1| (Box2::new(x0, y0, x1, y1), Fill::Solid);
    let eighths = |n: i32, whole: i32| (whole * n / 8).max(1);
    let quadrants = |mask: u32| {
        let (mx, my) = (cell.x0 + w / 2, cell.y0 + h / 2);
        let mut out = Vec::new();
        if mask & 1 != 0 {
            out.push(solid(cell.x0, cell.y0, mx, my));
        }
        if mask & 2 != 0 {
            out.push(solid(mx, cell.y0, cell.x1, my));
        }
        if mask & 4 != 0 {
            out.push(solid(cell.x0, my, mx, cell.y1));
        }
        if mask & 8 != 0 {
            out.push(solid(mx, my, cell.x1, cell.y1));
        }
        out
    };
    Some(match code {
        0x2580 => vec![solid(cell.x0, cell.y0, cell.x1, cell.y0 + h / 2)],
        0x2581..=0x2587 => {
            let n = (code - 0x2580) as i32;
            vec![solid(cell.x0, cell.y1 - eighths(n, h), cell.x1, cell.y1)]
        }
        0x2588 => vec![solid(cell.x0, cell.y0, cell.x1, cell.y1)],
        0x2589..=0x258f => {
            let n = 8 - (code - 0x2588) as i32;
            vec![solid(cell.x0, cell.y0, cell.x0 + eighths(n, w), cell.y1)]
        }
        0x2590 => vec![solid(cell.x0 + w / 2, cell.y0, cell.x1, cell.y1)],
        0x2591..=0x2593 => vec![(
            Box2::new(cell.x0, cell.y0, cell.x1, cell.y1),
            Fill::Quarters((code - 0x2590) as u8),
        )],
        0x2594 => vec![solid(cell.x0, cell.y0, cell.x1, cell.y0 + eighths(1, h))],
        0x2595 => vec![solid(cell.x1 - eighths(1, w), cell.y0, cell.x1, cell.y1)],
        0x2596 => quadrants(4),
        0x2597 => quadrants(8),
        0x2598 => quadrants(1),
        0x2599 => quadrants(1 | 4 | 8),
        0x259a => quadrants(1 | 8),
        0x259b => quadrants(1 | 2 | 4),
        0x259c => quadrants(1 | 2 | 8),
        0x259d => quadrants(2),
        0x259e => quadrants(2 | 4),
        _ => quadrants(2 | 4 | 8),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CELL: Box2 = Box2::new(0, 0, 10, 20);

    /// The set of lit pixels of `c` in a 10x20 cell with light 1 or 2.
    fn pixels(c: char, light: i32) -> Vec<Vec<bool>> {
        let mut grid = vec![vec![false; 10]; 20];
        for (b, _) in geometry(c, CELL, light).expect("geometric") {
            for y in b.y0..b.y1 {
                for x in b.x0..b.x1 {
                    grid[y as usize][x as usize] = true;
                }
            }
        }
        grid
    }

    #[test]
    fn a_light_line_is_one_pixel_through_the_middle() {
        let g = pixels('\u{2500}', 1);
        assert!(g[10].iter().all(|&p| p), "the whole row");
        assert_eq!(g.iter().flatten().filter(|&&p| p).count(), 10);
        let g = pixels('\u{2502}', 1);
        assert!(g.iter().all(|row| row[5]));
        assert_eq!(g.iter().flatten().filter(|&&p| p).count(), 20);
    }

    #[test]
    fn lines_join_across_neighbouring_cells() {
        // A horizontal line reaches both cell edges, a vertical one both the
        // top and the bottom: the next cell's line continues it exactly.
        let bounds = |c: char, light: i32| {
            let rects = geometry(c, CELL, light).unwrap();
            (
                rects.iter().map(|(b, _)| b.x0).min().unwrap(),
                rects.iter().map(|(b, _)| b.x1).max().unwrap(),
                rects.iter().map(|(b, _)| b.y0).min().unwrap(),
                rects.iter().map(|(b, _)| b.y1).max().unwrap(),
            )
        };
        for c in ['\u{2500}', '\u{2501}', '\u{2550}'] {
            for light in [1, 2, 3] {
                let (x0, x1, ..) = bounds(c, light);
                assert_eq!((x0, x1), (0, 10), "{c:?} at {light}");
            }
        }
        for c in ['\u{2502}', '\u{2503}', '\u{2551}'] {
            let (_, _, y0, y1) = bounds(c, 1);
            assert_eq!((y0, y1), (0, 20), "{c:?}");
        }
    }

    #[test]
    fn corners_fill_the_corner_pixel() {
        for c in ['\u{250c}', '\u{2510}', '\u{2514}', '\u{2518}', '\u{2523}', '\u{253c}'] {
            for light in [1, 2] {
                let g = pixels(c, light);
                assert!(g[10][5], "{c:?} lights the centre at {light}");
            }
        }
        let g = pixels('\u{250c}', 1);
        assert!(g[10][5] && g[10][9] && g[19][5], "right and down arms from the centre");
        assert!(!g[10][4] && !g[9][5], "no left or up arm");
    }

    #[test]
    fn heavy_is_twice_as_thick_as_light() {
        let light = geometry('\u{2500}', CELL, 2).unwrap()[0].0;
        let heavy = geometry('\u{2501}', CELL, 2).unwrap()[0].0;
        assert_eq!(heavy.height(), 2 * light.height());
        let center = |b: Box2| (b.y0 + b.y1) / 2;
        assert!((center(heavy) - center(light)).abs() <= 1, "both centred");
    }

    #[test]
    fn a_mixed_junction_covers_the_thicker_stroke() {
        // ┝: heavy right arm meeting a light vertical line.
        let g = pixels('\u{251d}', 1);
        assert!(g[10][5] && g[9][9] && g[10][9], "the heavy arm is two pixels tall");
        assert!(g.iter().all(|row| row[5]), "the vertical line runs the whole height");
    }

    #[test]
    fn double_lines_are_two_strokes_with_a_gap() {
        let g = pixels('\u{2550}', 1);
        let rows: Vec<usize> = (0..20).filter(|&y| g[y][0]).collect();
        assert_eq!(rows, [9, 11], "two strokes one light apart");
        let g = pixels('\u{2551}', 1);
        let cols: Vec<usize> = (0..10).filter(|&x| g[0][x]).collect();
        assert_eq!(cols, [4, 6]);
    }

    #[test]
    fn double_corners_keep_an_outer_and_an_inner_path() {
        // ╔: outer path along the top and left, inner path inside it.
        let g = pixels('\u{2554}', 1);
        assert!(g[9][4] && g[9][9], "the outer top stroke starts at the outer left stroke");
        assert!(g[11][6] && g[11][9] && !g[11][5], "the inner stroke starts at the inner vertical, leaving the gap");
        assert!(g[19][4] && g[19][6] && !g[19][5], "both verticals run down");
        assert!(!g[9][3] && !g[8][4], "nothing beyond the outer corner");
        // ╬: four open inner corners.
        let g = pixels('\u{256c}', 1);
        assert!(!g[10][5] && !g[9][5] && !g[11][5], "the centre is empty");
        assert!(g[9][9] && g[11][9] && g[0][4] && g[0][6]);
    }

    #[test]
    fn every_standard_character_has_geometry_but_the_diagonals() {
        for code in 0x2500u32..=0x259f {
            let c = char::from_u32(code).unwrap();
            let drawn = geometry(c, CELL, 1).is_some();
            assert_eq!(drawn, is_geometric(c), "{code:#x}");
            assert_eq!(drawn, !(0x2571..=0x2573).contains(&code), "{code:#x}");
        }
        assert!(!is_geometric('a') && !is_geometric('\u{25a0}'));
    }

    #[test]
    fn block_elements_cover_the_expected_fractions() {
        let area = |c: char| -> i32 {
            geometry(c, CELL, 1).unwrap().iter().map(|(b, _)| b.width() * b.height()).sum()
        };
        assert_eq!(area('\u{2588}'), 200, "full block");
        assert_eq!(area('\u{2580}'), 100, "upper half");
        assert_eq!(area('\u{2584}'), 100, "lower half");
        assert_eq!(area('\u{258c}'), 100, "left half");
        assert_eq!(area('\u{2590}'), 100, "right half");
        assert_eq!(area('\u{2581}'), 10 * 2, "a lower eighth");
        assert_eq!(area('\u{2589}'), 8 * 20, "seven eighths of ten pixels, rounded down");
        assert_eq!(area('\u{2596}'), 50, "one quadrant");
        assert_eq!(area('\u{259a}'), 100, "two diagonal quadrants");
        assert_eq!(area('\u{259b}'), 150);
        let shade = geometry('\u{2592}', CELL, 1).unwrap();
        assert_eq!(shade, [(CELL, Fill::Quarters(2))]);
        assert_eq!(geometry('\u{2591}', CELL, 1).unwrap()[0].1, Fill::Quarters(1));
        assert_eq!(geometry('\u{2593}', CELL, 1).unwrap()[0].1, Fill::Quarters(3));
    }

    #[test]
    fn it_is_safe_at_tiny_sizes() {
        for size in 1..6 {
            let cell = Box2::new(3, 4, 3 + size, 4 + size * 2);
            for code in 0x2500u32..=0x259f {
                for (b, _) in geometry(char::from_u32(code).unwrap(), cell, 3).unwrap_or_default() {
                    assert!(b.x0 >= cell.x0 && b.x1 <= cell.x1, "{code:#x} stays in its cell");
                    assert!(b.y0 >= cell.y0 && b.y1 <= cell.y1, "{code:#x} stays in its cell");
                    assert!(b.x1 > b.x0 && b.y1 > b.y0);
                }
            }
        }
    }
}
