//! Snapshots of the grid geometry: box drawing, columns, metrics.

use super::*;

#[test]
fn box_drawing_joins_across_cells_at_every_size() {
    const TABLE: &str = "\u{250c}\u{2500}\u{252c}\u{2500}\u{2510}\r\n\u{2502} \u{2502} \u{2502}\r\n\u{251c}\u{2500}\u{253c}\u{2500}\u{2524}\r\n\u{2502} \u{2502} \u{2502}\r\n\u{2514}\u{2500}\u{2534}\u{2500}\u{2518}\r\n\u{2554}\u{2550}\u{2566}\u{2550}\u{2557}\r\n\u{2551} \u{2551} \u{2551}\r\n\u{255a}\u{2550}\u{2569}\u{2550}\u{255d}\r\n\u{2501}\u{2503}\u{250f}\u{2513}\u{2517}\u{251b}\u{2588}\u{2593}\u{2592}\u{2591}\u{2580}\u{2584}\u{258c}\u{2590}\u{2596}\u{259f}";
    for (size, dpi, name) in [(15.0, 96, "term-box-15.png"), (22.0, 96, "term-box-22.png"), (15.0, 192, "term-box-2x.png"), (11.0, 96, "term-box-11.png")] {
        let mut term = Term::new(80, 19);
        term.feed(TABLE.as_bytes());
        let shot = Shot { size, dpi, ..Shot::default() };
        let image = shoot(term, &shot, name);
        let scheme = scheme_by_name("desktop", true);
        let m = image.m;
        // The horizontal line of the top row, through every column from the
        // corner to the corner, is one unbroken run of foreground pixels.
        let (x0, y0) = image.origin(0, 0);
        let y = y0 + m.line_h / 2;
        for x in x0 + m.cell_w / 2..x0 + 4 * m.cell_w + m.cell_w / 2 {
            assert_eq!(image.pixel(x, y), scheme.fg, "{name}: top line at x={x}");
        }
        // The first column's vertical line runs from the top corner to the
        // bottom corner without a gap, through the tees and the cross.
        let x = x0 + m.cell_w / 2;
        for row_y in y..y0 + 4 * m.line_h + m.line_h / 2 {
            assert_eq!(image.pixel(x, row_y), scheme.fg, "{name}: left line at y={row_y}");
        }
        // A full block fills its cell edge to edge.
        let (bx, by) = image.origin(6, 8);
        for (dx, dy) in [(0, 0), (m.cell_w - 1, 0), (0, m.line_h - 1), (m.cell_w - 1, m.line_h - 1)] {
            assert_eq!(image.pixel(bx + dx, by + dy), scheme.fg, "{name}: full block corner");
        }
        // The shades darken from dark (U+2593) to light (U+2591).
        let shade = |col| {
            let (x, y) = image.origin(col, 8);
            image.pixel(x + 2, y + 2).0
        };
        assert!(shade(7) > shade(8) && shade(8) > shade(9), "{name}: shades in order");
    }
}

#[test]
fn wide_and_fallback_glyphs_do_not_shift_the_columns_after_them() {
    let mut plain = Term::new(80, 19);
    plain.feed(b"a  cd");
    let mut wide = Term::new(80, 19);
    wide.feed("a\u{4e2d}cd".as_bytes());
    let mut accented = Term::new(80, 19);
    accented.feed("a\u{e9} cd".as_bytes());
    let bg = scheme_by_name("desktop", true).bg;
    let shot = Shot::default();
    let (plain, wide, accented) = (shoot(plain, &shot, "term-columns-plain.png"), shoot(wide, &shot, "term-columns-wide.png"), shoot(accented, &shot, "term-columns-fallback.png"));
    // "c" and "d" are in the same cells (4 and 5; 4 and 5) in every row.
    for image in [&plain, &wide, &accented] {
        assert!(image.has_ink(3, 0, 1, bg) && image.has_ink(4, 0, 1, bg), "c and d keep their cells");
    }
    let cell_ink = |s: &Shown, col| {
        let (x, y) = s.origin(col, 0);
        (0..s.m.line_h)
            .flat_map(|py| (0..s.m.cell_w).map(move |px| (px, py)))
            .filter(|&(px, py)| s.pixel(x + px, y + py) != bg)
            .count()
    };
    for col in [3, 4] {
        assert_eq!(cell_ink(&plain, col), cell_ink(&wide, col), "wide: column {col}");
        assert_eq!(cell_ink(&plain, col), cell_ink(&accented, col), "accent: column {col}");
    }
}

#[test]
fn a_fractional_advance_still_lands_every_glyph_in_its_cell() {
    // 17 px JetBrains Mono is 10.2 px a glyph: the cell is 10, the run is not
    // drawn as a string.
    let mut term = Term::new(80, 19);
    term.feed(b"MMMMMMMMMMMMMMMMMMMMMMMMMMMMMMMM\r\n");
    let shot = Shot { size: 17.0, ..Shot::default() };
    let image = shoot(term, &shot, "term-fractional-17.png");
    let bg = scheme_by_name("desktop", true).bg;
    assert!(!image.m.exact, "10.2 px advances are not exact");
    // The last M's ink is inside its own cell, not 6 px to the right of it.
    let last = 31;
    assert!(image.has_ink(last, 0, 1, bg));
    let (x, y) = image.origin(last + 1, 0);
    let past = (0..image.m.line_h).any(|py| (0..image.m.cell_w).any(|px| image.pixel(x + px, y + py) != bg));
    assert!(!past, "nothing beyond the last cell");
}

#[test]
fn zoom_and_scale_change_the_grid_not_the_design() {
    for (size, dpi, name) in [(15.0, 96, "term-size-15.png"), (24.0, 96, "term-size-24.png"), (11.0, 96, "term-size-11.png"), (15.0, 192, "term-2x.png")] {
        let mut term = Term::new(80, 40);
        term.feed(b"size and scale: Hello, World\r\n0123456789");
        let shot = Shot { size, dpi, ..Shot::default() };
        let image = shoot(term, &shot, name);
        let m = image.m;
        assert_eq!(m.scale, (dpi / 96) as i32);
        assert!(m.cell_w >= (0.55 * size * m.scale as f32) as i32 - 1, "{name}: the advance of M, not 0.6 em guessed");
        assert!(m.line_h >= (size * m.scale as f32) as i32);
        // The cursor sits after the last digit, exactly where the metrics say.
        let scheme = scheme_by_name("desktop", true);
        let (x, y) = image.origin(10, 1);
        assert_eq!(image.pixel(x + 1, y + m.line_h - 1), scheme.cursor, "{name}: cursor cell");
    }
}

#[test]
fn line_spacing_sets_the_row_height() {
    let mut heights = Vec::new();
    for (spacing, name) in [(100, "term-spacing-100.png"), (130, "term-spacing-130.png"), (180, "term-spacing-180.png")] {
        let mut term = Term::new(80, 40);
        term.feed(b"line one\r\nline two\r\nline three");
        let image = shoot(term, &Shot { spacing, ..Shot::default() }, name);
        heights.push(image.m.line_h);
    }
    assert!(heights[0] < heights[1] && heights[1] < heights[2], "{heights:?}");
}


/// A 3x3 table: every joint touches, at every line spacing and scale. The
/// verticals span the whole row (spacing included) and the horizontals run
/// through the row's centre.
#[test]
fn a_table_joins_across_rows_and_cells_at_every_spacing_and_scale() {
    const TABLE: &str = "\u{250c}\u{2500}\u{252c}\u{2500}\u{2510}\r\n\u{2502} \u{2502} \u{2502}\r\n\u{251c}\u{2500}\u{253c}\u{2500}\u{2524}\r\n\u{2502} \u{2502} \u{2502}\r\n\u{2514}\u{2500}\u{2534}\u{2500}\u{2518}";
    for dpi in [96, 192] {
        for spacing in [100, 130, 160] {
            let mut term = Term::new(80, 19);
            term.feed(TABLE.as_bytes());
            let shot = Shot { spacing, dpi, ..Shot::default() };
            let name = format!("term-table-{spacing}-{}x.png", dpi / 96);
            let image = shoot(term, &shot, &name);
            let (m, fg) = (image.m, scheme_by_name("desktop", true).fg);
            let tag = |what: &str| format!("{name}: {what}");
            // Every vertical (columns 0, 2 and 4, at the cell's centre) is one
            // unbroken line from the middle of the first row to the middle of
            // the last, so the rows meet with no gap of line spacing.
            for col in [0, 2, 4] {
                let (x, top) = image.origin(col, 0);
                let (x, from, to) = (x + m.cell_w / 2, top + m.line_h / 2, top + 4 * m.line_h + m.line_h / 2);
                for y in from..=to {
                    assert_eq!(image.pixel(x, y), fg, "{}", tag(&format!("vertical {col} at y={y}")));
                }
            }
            // The two plain rows' verticals reach both edges of their row.
            for row in [1, 3] {
                let (x, top) = image.origin(0, row);
                let x = x + m.cell_w / 2;
                assert_eq!(image.pixel(x, top), fg, "{}", tag("row top"));
                assert_eq!(image.pixel(x, top + m.line_h - 1), fg, "{}", tag("row bottom"));
            }
            // Every horizontal (rows 0, 2, 4) runs from the first corner to the
            // last through the row's centre.
            for row in [0, 2, 4] {
                let (left, top) = image.origin(0, row);
                let y = top + m.line_h / 2;
                for x in left + m.cell_w / 2..=left + 4 * m.cell_w + m.cell_w / 2 {
                    assert_eq!(image.pixel(x, y), fg, "{}", tag(&format!("horizontal row {row} at x={x}")));
                }
            }
        }
    }
}
