//! The panel menus' keyboard rules, one behaviour per test.

use super::*;
use RowKind::{Inert, Item, Parent};

/// The start menu's shape: categories, a disabled pinned row, power rows.
const MENU: [RowKind; 6] = [Parent, Parent, Inert, Item, Item, Item];

#[test]
fn down_from_nothing_selects_the_first_row_and_up_the_last() {
    assert_eq!(step(&MENU, None, NavKey::Down, false), Step::Select(0));
    assert_eq!(step(&MENU, None, NavKey::Up, false), Step::Select(5));
}

#[test]
fn up_and_down_skip_separators_and_disabled_rows() {
    assert_eq!(step(&MENU, Some(1), NavKey::Down, false), Step::Select(3));
    assert_eq!(step(&MENU, Some(3), NavKey::Up, false), Step::Select(1));
}

#[test]
fn up_and_down_wrap() {
    assert_eq!(step(&MENU, Some(5), NavKey::Down, false), Step::Select(0));
    assert_eq!(step(&MENU, Some(0), NavKey::Up, false), Step::Select(5));
    let tray = [Inert, Item, Inert];
    assert_eq!(step(&tray, Some(1), NavKey::Down, true), Step::Select(1));
}

#[test]
fn home_and_end_find_the_first_and_last_selectable_rows() {
    let rows = [Inert, Item, Item, Inert];
    assert_eq!(step(&rows, Some(2), NavKey::Home, false), Step::Select(1));
    assert_eq!(step(&rows, Some(1), NavKey::End, false), Step::Select(2));
}

#[test]
fn a_menu_with_nothing_to_choose_does_nothing() {
    assert_eq!(step(&[], None, NavKey::Down, false), Step::Nothing);
    assert_eq!(
        step(&[Inert, Inert], None, NavKey::Up, false),
        Step::Nothing
    );
    assert_eq!(step(&[Inert], None, NavKey::Enter, false), Step::Nothing);
}

#[test]
fn right_and_enter_open_a_submenu() {
    assert_eq!(
        step(&MENU, Some(1), NavKey::Right, false),
        Step::OpenChild(1)
    );
    assert_eq!(
        step(&MENU, Some(0), NavKey::Enter, false),
        Step::OpenChild(0)
    );
}

#[test]
fn right_on_an_ordinary_row_does_nothing() {
    assert_eq!(step(&MENU, Some(3), NavKey::Right, false), Step::Nothing);
    assert_eq!(step(&MENU, None, NavKey::Right, false), Step::Nothing);
}

#[test]
fn enter_activates_an_ordinary_row_but_never_a_disabled_one() {
    assert_eq!(
        step(&MENU, Some(4), NavKey::Enter, false),
        Step::Activate(4)
    );
    assert_eq!(step(&MENU, Some(2), NavKey::Enter, false), Step::Nothing);
    assert_eq!(step(&MENU, None, NavKey::Enter, false), Step::Nothing);
}

#[test]
fn left_and_escape_close_one_level() {
    assert_eq!(step(&MENU, Some(0), NavKey::Left, true), Step::Back);
    assert_eq!(step(&MENU, Some(0), NavKey::Escape, true), Step::Back);
    assert_eq!(step(&MENU, Some(0), NavKey::Left, false), Step::Nothing);
    assert_eq!(step(&MENU, Some(0), NavKey::Escape, false), Step::Close);
}

#[test]
fn a_selection_out_of_range_counts_as_none() {
    assert_eq!(step(&MENU, Some(99), NavKey::Down, false), Step::Select(0));
    assert_eq!(step(&MENU, Some(99), NavKey::Enter, false), Step::Nothing);
}

#[test]
fn the_tray_moves_left_and_right_and_wraps() {
    assert_eq!(tray_step(3, Some(0), NavKey::Right), TrayStep::Select(1));
    assert_eq!(tray_step(3, Some(2), NavKey::Right), TrayStep::Select(0));
    assert_eq!(tray_step(3, Some(0), NavKey::Left), TrayStep::Select(2));
    assert_eq!(tray_step(3, None, NavKey::Right), TrayStep::Select(0));
    assert_eq!(tray_step(3, Some(1), NavKey::End), TrayStep::Select(2));
    assert_eq!(tray_step(3, Some(1), NavKey::Home), TrayStep::Select(0));
}

#[test]
fn the_tray_activates_or_opens_the_lit_cell() {
    assert_eq!(tray_step(2, Some(1), NavKey::Enter), TrayStep::Activate(1));
    assert_eq!(tray_step(2, Some(1), NavKey::Up), TrayStep::Menu(1));
    assert_eq!(tray_step(2, Some(0), NavKey::Menu), TrayStep::Menu(0));
    assert_eq!(tray_step(2, None, NavKey::Enter), TrayStep::Nothing);
}

#[test]
fn escape_or_an_empty_tray_leaves_it() {
    assert_eq!(tray_step(2, Some(0), NavKey::Escape), TrayStep::Leave);
    assert_eq!(tray_step(0, None, NavKey::Right), TrayStep::Leave);
}
