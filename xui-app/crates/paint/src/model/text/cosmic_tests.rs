//! The real rasterizer against the repository's Liberation fonts.

use std::path::PathBuf;

use super::*;
use crate::model::rect::Rect;

fn fonts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../assets/fonts")
}

fn rasterizer() -> CosmicRasterizer {
    let mut r = CosmicRasterizer::new();
    for name in [
        "liberation/LiberationSans-Regular.ttf",
        "liberation/LiberationSans-Bold.ttf",
        "liberation/LiberationSans-Italic.ttf",
        "liberation/LiberationSerif-Regular.ttf",
        "DroidSans.ttf",
    ] {
        let path = fonts_dir().join(name);
        r.register_font(std::fs::read(&path).unwrap_or_else(|e| panic!("{path:?}: {e}")));
    }
    r
}

fn style() -> TextStyle {
    TextStyle {
        size: 32.0,
        ..TextStyle::default()
    }
}

/// The ink's bounds within rows `y0..y1` of the mask.
fn ink(layout: &TextCoverage, y0: i32, y1: i32) -> Rect {
    let mask = &layout.mask;
    let mut bounds = Rect::EMPTY;
    for y in y0.max(mask.rect().y0)..y1.min(mask.rect().y1) {
        for x in mask.rect().x0..mask.rect().x1 {
            if mask.get(x, y) != 0 {
                bounds = bounds.union(Rect::new(x, y, 1, 1));
            }
        }
    }
    bounds
}

#[test]
fn hello_has_sensible_coverage() {
    let layout = rasterizer().layout("Hello", &style());
    let mask = &layout.mask;
    assert!(mask.total() > 0, "ink");
    let bounds = mask.ink_bounds();
    assert!(!bounds.is_empty());
    assert!(
        mask.rect().intersect(bounds) == bounds,
        "ink inside the mask"
    );
    // 32 px text: five glyphs are roughly 60-100 px wide, one line tall.
    assert!((50..110).contains(&(bounds.width() as i32)), "{bounds:?}");
    assert!(bounds.height() >= 18 && bounds.height() <= 42, "{bounds:?}");
    // It is antialiased: some partial coverage.
    assert!(mask.data().iter().any(|&v| v != 0 && v != 255));
    assert_eq!(layout.lines.len(), 1);
    let line = &layout.lines[0];
    assert_eq!(line.carets.len(), 6);
    assert_eq!(line.carets[0], 0.0);
    assert!(
        line.carets.windows(2).all(|w| w[1] > w[0]),
        "{:?}",
        line.carets
    );
    let ink_right = bounds.x1 as f32;
    assert!(
        (line.carets[5] - ink_right).abs() < 8.0,
        "caret end {} vs ink {ink_right}",
        line.carets[5]
    );
    // The baseline sits inside the line, below the ink's middle.
    assert!(line.baseline > line.top && line.baseline < line.top + line.height);
    assert!((bounds.y1 as f32) <= line.top + line.height + 1.0);
}

#[test]
fn bold_is_wider_and_italic_differs() {
    let r = rasterizer();
    let regular = r.layout("Hello World", &style());
    let bold = r.layout(
        "Hello World",
        &TextStyle {
            bold: true,
            ..style()
        },
    );
    let italic = r.layout(
        "Hello World",
        &TextStyle {
            italic: true,
            ..style()
        },
    );
    let width = |l: &TextCoverage| l.lines[0].carets.last().copied().unwrap();
    assert!(
        width(&bold) > width(&regular) + 2.0,
        "{} vs {}",
        width(&bold),
        width(&regular)
    );
    assert!(bold.mask.total() > regular.mask.total());
    assert!(italic.mask != regular.mask);
}

#[test]
fn centre_alignment_offsets_short_lines() {
    let r = rasterizer();
    let text = "Hello World\nHi";
    let left = r.layout(text, &style());
    let centre = r.layout(
        text,
        &TextStyle {
            align: TextAlign::Center,
            ..style()
        },
    );
    let right = r.layout(
        text,
        &TextStyle {
            align: TextAlign::Right,
            ..style()
        },
    );
    let second = |l: &TextCoverage| {
        let line = &l.lines[1];
        ink(l, line.top as i32, (line.top + line.height) as i32)
    };
    let first = |l: &TextCoverage| {
        let line = &l.lines[0];
        ink(l, line.top as i32, (line.top + line.height) as i32)
    };
    assert_eq!(
        first(&left).x0,
        first(&centre).x0,
        "the widest line does not move"
    );
    let (a, b, c) = (second(&left).x0, second(&centre).x0, second(&right).x0);
    assert!(b > a + 20 && c > b + 20, "left {a}, centre {b}, right {c}");
    // The carets move with the ink.
    assert!(centre.lines[1].carets[0] > left.lines[1].carets[0] + 20.0);
    let block = left.lines[0].carets[11];
    let mid = (centre.lines[1].carets[0] + centre.lines[1].carets[2]) / 2.0;
    assert!(
        (mid - block / 2.0).abs() < 2.0,
        "centred: {mid} vs {}",
        block / 2.0
    );
    let end = right.lines[1].carets[2];
    assert!((end - block).abs() < 1.5, "right edge {end} vs {block}");
}

#[test]
fn lines_stack_and_carets_follow_each_line() {
    let layout = rasterizer().layout("ab\n\ncde", &style());
    assert_eq!(layout.lines.len(), 3);
    let starts: Vec<usize> = layout.lines.iter().map(|l| l.start).collect();
    assert_eq!(starts, [0, 3, 4]);
    assert_eq!(
        layout.lines[1].carets.len(),
        1,
        "an empty line has just its start"
    );
    assert_eq!(layout.lines[2].carets.len(), 4);
    assert!(layout.lines[1].top > layout.lines[0].top && layout.lines[2].top > layout.lines[1].top);
    assert!(
        layout.mask.rect().height() as f32 >= layout.lines[2].top + layout.lines[2].height - 1.0
    );
}

#[test]
fn underline_adds_a_bar_below_the_baseline() {
    let r = rasterizer();
    let plain = r.layout("Hill", &style());
    let underlined = r.layout(
        "Hill",
        &TextStyle {
            underline: true,
            ..style()
        },
    );
    let baseline = plain.lines[0].baseline.round() as i32;
    let below = |l: &TextCoverage| ink(l, baseline + 2, baseline + 8);
    assert!(below(&plain).is_empty(), "no descenders in Hill");
    let bar = below(&underlined);
    assert!(!bar.is_empty());
    assert!(
        bar.width() as f32 >= plain.lines[0].carets[4] - 2.0,
        "the bar spans the line"
    );
}

#[test]
fn antialias_off_is_hard_edged() {
    let hard = rasterizer().layout(
        "Sample",
        &TextStyle {
            antialias: false,
            ..style()
        },
    );
    assert!(hard.mask.total() > 0);
    assert!(hard.mask.data().iter().all(|&v| v == 0 || v == 255));
}

#[test]
fn families_unknown_names_and_no_fonts() {
    let r = rasterizer();
    let families = r.families();
    assert!(
        families.iter().any(|f| f == "Liberation Sans"),
        "{families:?}"
    );
    assert!(families.iter().any(|f| f == "Liberation Serif"));
    // An unknown family falls back instead of drawing nothing.
    let unknown = r.layout(
        "Hi",
        &TextStyle {
            family: "No Such Font".into(),
            ..style()
        },
    );
    assert!(unknown.mask.total() > 0);
    // Serif and sans differ.
    let serif = r.layout(
        "Hi",
        &TextStyle {
            family: "Liberation Serif".into(),
            ..style()
        },
    );
    assert!(serif.mask != r.layout("Hi", &style()).mask);

    let bare = CosmicRasterizer::new();
    let blank = bare.layout("Hello", &style());
    assert!(blank.mask.is_empty() && blank.lines.len() == 1);
    // Garbage font bytes are skipped.
    let mut junk = CosmicRasterizer::new();
    junk.register_font(vec![1, 2, 3, 4]);
    assert!(junk.layout("x", &style()).mask.is_empty());
}

#[test]
fn hostile_styles_do_not_blow_up() {
    let r = rasterizer();
    let huge = r.layout(
        "W",
        &TextStyle {
            size: 1.0e9,
            ..style()
        },
    );
    assert!(huge.mask.rect().width() < 4096 * 4 && huge.mask.rect().height() < 4096 * 4);
    let tiny = r.layout(
        "W",
        &TextStyle {
            size: -5.0,
            ..style()
        },
    );
    assert!(tiny.lines.len() == 1);
    let nan = r.layout(
        "W",
        &TextStyle {
            size: f32::NAN,
            ..style()
        },
    );
    assert_eq!(nan.lines.len(), 1);
}

#[test]
fn an_edit_commits_to_positioned_coverage() {
    let r = rasterizer();
    let mut edit = TextEdit::new((100, 50), style());
    edit.insert_str("Hi");
    edit.newline();
    edit.insert('!');
    let committed = edit.commit(&r).expect("ink");
    assert!(committed.rect().x0 == 100 && committed.rect().y0 == 50);
    let (x, y, h) = edit.caret_rect(&r).unwrap();
    assert!(x > 100.0 && y > 50.0 && h > 20.0);
    edit.home();
    assert_eq!(edit.caret_rect(&r).unwrap().0, 100.0);
}
