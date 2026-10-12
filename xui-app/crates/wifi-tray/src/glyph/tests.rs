use super::*;

fn all() -> Vec<Glyph> {
    let mut glyphs: Vec<Glyph> = (1..=4).map(Glyph::Bars).collect();
    glyphs.extend((0..4).map(Glyph::Connecting));
    glyphs.push(Glyph::Disconnected);
    glyphs.push(Glyph::Unavailable);
    glyphs
}

#[test]
fn masks_are_32_by_32_straight_rgba_with_white_ink() {
    for glyph in all() {
        let mask = glyph.mask();
        assert_eq!((mask.width, mask.height), (SIDE, SIDE));
        assert_eq!(mask.rgba.len(), (SIDE * SIDE * 4) as usize);
        assert!(mask.rgba.chunks_exact(4).all(|p| p[..3] == [255, 255, 255]));
        let icon = mask.icon();
        assert!(icon.mask.is_some() && icon.lucide.is_none() && icon.pixels.is_empty());
    }
}

#[test]
fn every_glyph_looks_different() {
    let masks: Vec<(Glyph, Mask)> = all().into_iter().map(|g| (g, g.mask())).collect();
    for (i, (a, mask_a)) in masks.iter().enumerate() {
        for (b, mask_b) in &masks[i + 1..] {
            assert_ne!(mask_a, mask_b, "{a:?} and {b:?} draw the same");
        }
    }
}

#[test]
fn more_bars_mean_more_ink() {
    let ink: Vec<u64> = (1..=4).map(|n| Glyph::Bars(n).mask().ink()).collect();
    assert!(ink.windows(2).all(|w| w[0] < w[1]), "{ink:?}");
}

#[test]
fn a_lit_segment_is_solid_and_an_unlit_one_faint() {
    // The dot, and a point on the outer arc straight above it.
    let (dot_x, dot_y) = (16, 26);
    let (arc_x, arc_y) = (16, (CENTRE.1 - ARC_RADII[2]) as u32);
    let one = Glyph::Bars(1).mask();
    assert_eq!(one.alpha(dot_x, dot_y), 255);
    let faint = one.alpha(arc_x, arc_y);
    assert!((1..255).contains(&u32::from(faint)), "faint arc: {faint}");
    assert_eq!(Glyph::Bars(4).mask().alpha(arc_x, arc_y), 255);
    // The corner stays clear.
    assert_eq!(Glyph::Bars(4).mask().alpha(0, 31), 0);
}

#[test]
fn disconnected_has_no_solid_pixel_and_connecting_is_dimmer_than_connected() {
    let disconnected = Glyph::Disconnected.mask();
    assert!(disconnected.rgba.chunks_exact(4).all(|p| p[3] < 255));
    assert!(disconnected.ink() > 0);
    let joining = Glyph::Connecting(3).mask();
    assert!(joining.rgba.chunks_exact(4).all(|p| p[3] < 255));
    assert!(joining.ink() > disconnected.ink());
    assert!(joining.ink() < Glyph::Bars(4).mask().ink());
}

#[test]
fn unavailable_is_struck_through_with_a_gap() {
    let struck = Glyph::Unavailable.mask();
    // The middle of the strike is solid.
    assert_eq!(struck.alpha(16, 16), 255);
    // The fan under it is cleared beside the strike: the faint outer arc
    // crossed by the diagonal loses pixels compared with the plain fan.
    assert!(struck.ink() != Glyph::Disconnected.mask().ink());
}

#[test]
fn connected_never_looks_like_disconnected() {
    assert_eq!(Glyph::connected(0), Glyph::Bars(1));
    assert_eq!(Glyph::connected(9), Glyph::Bars(4));
    assert_eq!(Glyph::connected(3), Glyph::Bars(3));
}

#[test]
fn glyph_names_are_stable_evidence() {
    assert_eq!(Glyph::Bars(3).name(), "bars3");
    assert_eq!(Glyph::Disconnected.name(), "disconnected");
    assert_eq!(Glyph::Connecting(2).name(), "connecting2");
    assert_eq!(Glyph::Unavailable.name(), "unavailable");
}

#[test]
fn out_of_range_glyphs_are_clamped_not_a_panic() {
    assert_eq!(Glyph::Bars(200).mask(), Glyph::Bars(4).mask());
    assert_eq!(Glyph::Connecting(200).mask(), Glyph::Connecting(3).mask());
}
