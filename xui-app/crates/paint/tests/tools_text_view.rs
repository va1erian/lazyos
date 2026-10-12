//! The Text tool typed through the window with real key events and real
//! Liberation glyphs (feature `text`).
#![cfg(feature = "text")]

mod common;

use xui_core::backend::Event;
use xui_core::image::Image;
use xui_core::message::{Key, MouseButton};
use xui_paint::Msg;
use xui_paint::model::Tool;

use common::driver::*;
use common::setup::{Look, centre, liberation, probe, session_look};

fn ink(image: &Image, rect: (i32, i32, i32, i32)) -> usize {
    let (l, t, r, b) = rect;
    (t..b)
        .flat_map(|y| (l..r).map(move |x| (x, y)))
        .filter(|&(x, y)| !is(image, x as u32, y as u32, WHITE))
        .count()
}

fn char_key(stage: &Stg<'_>, c: char) {
    stage.inject(Event::Char(c));
}

fn type_text(stage: &Stg<'_>, text: &str) {
    for c in text.chars() {
        char_key(stage, c);
    }
}

fn look() -> Look {
    Look::plain().with_text(liberation())
}

#[test]
fn typing_shows_live_text_and_letters_do_not_switch_tools() {
    let image = session_look(look(), |stage, observer, parts, _| {
        press(stage, Key::T);
        assert_eq!(observer.borrow().tool, Tool::Text);
        let (x, y) = parts.areas.canvas_at(40, 40);
        click(stage, (x, y), MouseButton::Left);
        assert!(observer.borrow().text_editing, "a click places the caret");
        // `b`, `e` and `p` are tool keys; while typing they are letters.
        type_text(stage, "Beep");
        press(stage, Key::from_code(0x42)); // B again, as a KeyDown
        assert_eq!(observer.borrow().tool, Tool::Text);
        assert!(observer.borrow().text_editing);
        assert!(!observer.borrow().can_undo, "typing is not committed");
    });
    let areas = session_areas();
    let (x, y) = areas.canvas_at(40, 40);
    assert!(
        ink(&image, (x, y, x + 80, y + 30)) > 100,
        "glyphs are on screen"
    );
}

#[test]
fn enter_arrows_backspace_and_delete_edit_the_text() {
    let image = session_look(look(), |stage, observer, parts, _| {
        press(stage, Key::T);
        let (x, y) = parts.areas.canvas_at(30, 30);
        click(stage, (x, y), MouseButton::Left);
        type_text(stage, "ab");
        press(stage, Key::RETURN);
        type_text(stage, "cd");
        press(stage, Key::UP);
        press(stage, Key::END);
        press(stage, Key::BACK);
        press(stage, Key::HOME);
        press(stage, Key::DELETE);
        press(stage, Key::DOWN);
        press(stage, Key::LEFT);
        press(stage, Key::RIGHT);
        assert!(observer.borrow().text_editing);
        ctrl(stage, Key::RETURN);
        assert!(!observer.borrow().text_editing, "Ctrl+Enter commits");
        assert!(observer.borrow().can_undo);
        assert_eq!(observer.borrow().history.0, 1, "one entry");
    });
    let areas = session_areas();
    let (x, y) = areas.canvas_at(30, 30);
    assert!(
        ink(&image, (x, y + 28, x + 40, y + 62)) > 40,
        "the second line"
    );
}

#[test]
fn esc_drops_the_text_and_a_click_outside_commits_it() {
    session_look(look(), |stage, observer, parts, _| {
        press(stage, Key::T);
        let (x, y) = parts.areas.canvas_at(30, 30);
        click(stage, (x, y), MouseButton::Left);
        type_text(stage, "gone");
        press(stage, Key::ESCAPE);
        assert!(!observer.borrow().text_editing);
        assert!(!observer.borrow().can_undo, "Esc leaves no trace");

        click(stage, (x, y), MouseButton::Left);
        type_text(stage, "kept");
        let (x2, y2) = parts.areas.canvas_at(200, 150);
        click(stage, (x2, y2), MouseButton::Left);
        assert_eq!(observer.borrow().history.0, 1, "a click outside commits");
        assert!(observer.borrow().text_editing, "and starts a new block");
        press(stage, Key::H);
        assert_eq!(observer.borrow().tool, Tool::Text, "H is a letter now");
        assert!(observer.borrow().text_editing);
        stage.emit(Msg::Tool(Tool::Pan));
        assert!(!observer.borrow().text_editing);
        assert_eq!(observer.borrow().history.0, 1, "no ink, nothing to commit");
    });
}

#[test]
fn a_tool_change_through_the_grid_commits_the_text() {
    session_look(look(), |stage, observer, parts, _| {
        press(stage, Key::T);
        let (x, y) = parts.areas.canvas_at(30, 30);
        click(stage, (x, y), MouseButton::Left);
        type_text(stage, "Hello");
        stage.emit(Msg::Tool(Tool::Brush));
        assert_eq!(observer.borrow().history.0, 1);
        assert!(!observer.borrow().text_editing);
        ctrl(stage, Key::Z);
        assert_eq!(observer.borrow().history.0, 0, "the text is one undo step");
    });
}

#[test]
fn the_handle_drags_the_block_with_the_mouse() {
    let image = session_look(look(), |stage, observer, parts, _| {
        press(stage, Key::T);
        let areas = &parts.areas;
        let (x, y) = areas.canvas_at(30, 40);
        click(stage, (x, y), MouseButton::Left);
        type_text(stage, "Hi");
        // The handle sits just above and left of the block's corner.
        let handle = areas.canvas_at(25, 35);
        let target = areas.canvas_at(105, 115);
        drag(stage, handle, target, MouseButton::Left);
        assert!(observer.borrow().text_editing, "moving keeps it editable");
        assert!(!observer.borrow().can_undo);
        ctrl(stage, Key::RETURN);
        assert_eq!(observer.borrow().history.0, 1);
    });
    let areas = session_areas();
    assert_eq!(
        ink(
            &image,
            (
                areas.canvas_at(28, 38).0,
                areas.canvas_at(28, 38).1,
                areas.canvas_at(70, 70).0,
                areas.canvas_at(70, 70).1
            )
        ),
        0,
        "gone from the old place"
    );
    let (a, b) = (areas.canvas_at(105, 115), areas.canvas_at(150, 150));
    assert!(
        ink(&image, (a.0, a.1, b.0, b.1)) > 60,
        "drawn at the new one"
    );
}

#[test]
fn a_check_box_hands_the_keyboard_back_to_the_text() {
    session_look(look(), |stage, observer, parts, probed| {
        press(stage, Key::T);
        probe(stage, probed);
        let (x, y) = parts.areas.canvas_at(30, 30);
        click(stage, (x, y), MouseButton::Left);
        type_text(stage, "a");
        click(stage, centre(probed, "bold"), MouseButton::Left);
        // The next key is a letter for the text, not a tool shortcut.
        press(stage, Key::from_code(0x45)); // E
        stage.inject(Event::Char('e'));
        assert_eq!(observer.borrow().tool, Tool::Text);
        assert!(observer.borrow().text_editing);
        // And Backspace still edits it (not the check box).
        press(stage, Key::BACK);
        ctrl(stage, Key::RETURN);
        assert_eq!(observer.borrow().history.0, 1);
    });
}

#[test]
fn the_font_controls_restyle_the_live_text() {
    let plain = session_look(look(), |stage, _, parts, _| {
        press(stage, Key::T);
        let (x, y) = parts.areas.canvas_at(30, 30);
        click(stage, (x, y), MouseButton::Left);
        type_text(stage, "Style");
    });
    let bold = session_look(look(), |stage, _, parts, probed| {
        press(stage, Key::T);
        probe(stage, probed);
        click(stage, centre(probed, "bold"), MouseButton::Left);
        let (x, y) = parts.areas.canvas_at(30, 30);
        click(stage, (x, y), MouseButton::Left);
        type_text(stage, "Style");
    });
    let areas = session_areas();
    let (x, y) = areas.canvas_at(30, 30);
    let rect = (x, y, x + 120, y + 40);
    assert!(
        ink(&bold, rect) > ink(&plain, rect),
        "the bold check box thickens the glyphs"
    );
}
