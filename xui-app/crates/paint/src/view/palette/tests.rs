use std::collections::HashSet;

use super::*;

fn geometry(width: i32, dpi: u32) -> Geometry {
    Geometry {
        bounds: Rect::new(0, 0, width, 400),
        dpi,
    }
}

fn centre(rect: Rect) -> Point {
    Point::new((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2)
}

#[test]
fn the_palette_has_32_distinct_opaque_colours() {
    assert_eq!(PALETTE.len(), 32);
    let unique: HashSet<Pixel> = PALETTE.iter().copied().collect();
    assert_eq!(unique.len(), PALETTE.len());
    assert!(PALETTE.iter().all(|color| color[3] == 255));
    assert_eq!(PALETTE[0], [0, 0, 0, 255], "black first");
    assert_eq!(
        PALETTE[15],
        [255, 255, 255, 255],
        "white closes the classic 16"
    );
}

#[test]
fn the_grid_wraps_to_the_panel_width() {
    let wide = geometry(160, 96);
    assert_eq!(wide.columns(), 8, "eight to a row in the right column");
    let narrow = geometry(100, 96);
    assert!(narrow.columns() < 8);
    assert!(narrow.height() > wide.height(), "fewer columns, more rows");
}

#[test]
fn every_colour_cell_fits_inside_the_panel_and_none_overlap() {
    for dpi in [96, 192] {
        let width = Dip(160.0).to_px(dpi).value();
        let g = geometry(width, dpi);
        let rects: Vec<Rect> = (0..COLORS).map(|i| g.color(i).unwrap()).collect();
        for rect in &rects {
            assert!(rect.left >= 0 && rect.right <= width, "{rect:?} at {dpi}");
        }
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let overlap =
                    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;
                assert!(!overlap, "{a:?} overlaps {b:?}");
            }
        }
        assert!(g.color(COLORS).is_none());
        let last = rects.last().unwrap();
        assert!(
            g.height() >= last.bottom,
            "the measured height holds the grid"
        );
    }
}

#[test]
fn hits_find_each_part_and_the_primary_is_in_front() {
    let g = geometry(160, 96);
    assert_eq!(g.hit(centre(g.primary())), Some(Hit::Primary));
    assert_eq!(g.hit(centre(g.swap())), Some(Hit::Swap));
    assert_eq!(g.hit(centre(g.reset())), Some(Hit::Reset));
    assert_eq!(g.hit(centre(g.color(9).unwrap())), Some(Hit::Color(9)));
    // The corner of the secondary swatch that the primary does not cover.
    let secondary = g.secondary();
    let uncovered = Point::new(secondary.right - 2, secondary.bottom - 2);
    assert_eq!(g.hit(uncovered), Some(Hit::Secondary));
    // Where they overlap the primary wins.
    let overlap = Point::new(g.primary().right - 2, g.primary().bottom - 2);
    assert_eq!(g.hit(overlap), Some(Hit::Primary));
    assert_eq!(g.hit(Point::new(900, 900)), None);
}

#[test]
fn the_buttons_do_not_cover_the_swatches_or_the_grid() {
    for dpi in [96, 192] {
        let g = geometry(Dip(160.0).to_px(dpi).value(), dpi);
        for button in [g.swap(), g.reset()] {
            assert!(
                button.left >= g.secondary().right,
                "{button:?} clears the swatches"
            );
            assert!(
                button.bottom <= g.header_bottom(),
                "{button:?} stays in the header"
            );
        }
        assert!(g.swap().bottom <= g.reset().top, "swap above reset");
    }
}
