use super::*;

const H: i32 = 30;

#[test]
fn tabs_share_the_width_up_to_a_limit_and_never_shrink_past_a_floor() {
    let wide = Geometry::new(2, 1000, 96);
    assert_eq!(wide.tab, 220, "a few tabs do not stretch");
    let tight = Geometry::new(4, 600, 96);
    assert_eq!(tight.tab, (600 - 32) / 4);
    let crowded = Geometry::new(40, 600, 96);
    assert_eq!(crowded.tab, 44, "they overflow rather than vanish");
    assert!(!crowded.close_all && tight.close_all);
}

#[test]
fn a_click_lands_on_a_tab_its_close_button_or_the_new_tab_button() {
    let g = Geometry::new(3, 800, 96);
    assert_eq!(g.hit(10, 10, H), Hit::Tab(0));
    assert_eq!(g.hit(g.tab + 10, 10, H), Hit::Tab(1));
    let close = g.close_rect(2, H);
    assert_eq!(g.hit(close.left + 2, close.top + 2, H), Hit::Close(2));
    // Just left of the close button is the tab again.
    assert_eq!(g.hit(close.left - 3, close.top + 2, H), Hit::Tab(2));
    let plus = g.plus_rect(H);
    assert_eq!(g.hit(plus.left + 2, 5, H), Hit::New);
    assert_eq!(g.hit(plus.right + 5, 5, H), Hit::Nothing);
    assert_eq!(g.hit(-1, 5, H), Hit::Nothing);
    assert_eq!(g.hit(10, H, H), Hit::Nothing);
}

#[test]
fn the_new_tab_button_stays_in_view_when_the_tabs_overflow() {
    let g = Geometry::new(40, 600, 96);
    let plus = g.plus_rect(H);
    assert_eq!(plus.right, 600);
    assert_eq!(g.hit(plus.left + 1, 5, H), Hit::New);
    // A tab past the right edge cannot be hit.
    assert_eq!(g.hit(700, 5, H), Hit::Nothing);
}

#[test]
fn a_long_title_is_cut_with_an_ellipsis_and_a_short_one_is_whole() {
    assert_eq!(fitted("Short", 200, 7), "Short");
    let cut = fitted("A very long page title indeed", 70, 7);
    assert_eq!(cut.chars().count(), 10);
    assert!(cut.ends_with('\u{2026}'));
    assert_eq!(fitted("x", 0, 7), "x");
}
