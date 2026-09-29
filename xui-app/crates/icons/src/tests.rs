use std::collections::HashSet;

use xui_core::backend::PathSeg;

use crate::{Category, Palette, Tone, Village};

/// How far outside the 32-unit grid a coordinate may stray (a stroke's overhang
/// and the modem antenna's radio waves).
const SLACK: f32 = 4.0;

fn coordinates(seg: &PathSeg) -> Vec<f32> {
    match *seg {
        PathSeg::MoveTo(x, y) | PathSeg::LineTo(x, y) => vec![x, y],
        PathSeg::CubicTo(a, b, c, d, e, f) => vec![a, b, c, d, e, f],
        PathSeg::Close => Vec::new(),
    }
}

#[test]
fn the_set_has_36_uniquely_named_icons() {
    assert_eq!(Village::ALL.len(), 36);
    let names: HashSet<_> = Village::ALL.iter().map(|icon| icon.name()).collect();
    assert_eq!(names.len(), 36);
}

#[test]
fn each_category_has_its_share() {
    let count = |category| {
        Village::ALL
            .iter()
            .filter(|icon| icon.category() == category)
            .count()
    };
    assert_eq!(count(Category::Hardware), 12);
    assert_eq!(count(Category::Files), 6);
    assert_eq!(count(Category::System), 12);
    assert_eq!(count(Category::Toolkit), 6);
}

#[test]
fn every_shape_is_painted_and_stays_near_the_grid() {
    for &icon in Village::ALL {
        assert!(!icon.shapes().is_empty(), "{icon:?} has no shapes");
        for shape in icon.shapes() {
            assert!(
                shape.fill.is_some() || shape.line.is_some(),
                "{icon:?} has an invisible shape"
            );
            assert!(matches!(shape.path.first(), Some(PathSeg::MoveTo(..))));
            for value in shape.path.iter().flat_map(coordinates) {
                assert!(
                    (-SLACK..=crate::GRID + SLACK).contains(&value),
                    "{icon:?} strays to {value}"
                );
            }
        }
    }
}

#[test]
fn a_palette_retints_one_tone_only() {
    let red = xui_core::backend::Rgba::rgb(0xFF, 0, 0);
    let palette = Palette::GLOBAL_VILLAGE.with(Tone::Teal, red);
    assert_eq!(palette.get(Tone::Teal), red);
    assert_eq!(
        palette.get(Tone::Rose),
        Palette::GLOBAL_VILLAGE.get(Tone::Rose)
    );
}
