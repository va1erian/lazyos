use super::*;

#[test]
fn the_colour_grid_wraps_to_more_rows_when_narrow() {
    assert_eq!(color_rows(PALETTE.len(), 800, 96), 1);
    assert!(color_rows(PALETTE.len(), 200, 96) > 1);
}

#[test]
fn a_hit_finds_a_colour_and_the_swap() {
    let bounds = Rect::new(0, 0, 800, 24);
    assert_eq!(hit(bounds, 96, Point::new(2, 2)).1, Some(Side::Primary));
    assert_eq!(hit(bounds, 96, Point::new(2, 20)).1, Some(Side::Secondary));
    let first = color_rect(bounds, 96, 0).unwrap();
    assert_eq!(
        hit(bounds, 96, Point::new(first.left + 2, first.top + 2)).0,
        Some(0)
    );
    assert!(hit(bounds, 96, Point::new(900, 900)).0.is_none());
}
