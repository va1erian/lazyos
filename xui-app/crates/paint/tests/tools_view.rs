//! The tools driven through the window: real mouse drags and key presses on
//! the canvas, the options bar's controls, Zoom and Pan, Esc mid-drag, and
//! (feature `text`) the Text tool typed with real key events and real glyphs.

mod common;

use xui_core::backend::Event;
use xui_core::image::Image;
use xui_core::message::{Key, Modifiers, MouseButton};
use xui_paint::Msg;
use xui_paint::model::Tool;
use xui_paint::view::OptMsg;

use common::driver::*;
use common::setup::{Look, centre, probe, session_look};

const SHIFT: Modifiers = Modifiers {
    shift: true,
    ..Modifiers::NONE
};

fn down_with(x: i32, y: i32, modifiers: Modifiers) -> Event {
    Event::MouseDown {
        x,
        y,
        button: MouseButton::Left,
        modifiers,
    }
}

fn move_with(x: i32, y: i32, modifiers: Modifiers) -> Event {
    Event::MouseMove { x, y, modifiers }
}

fn up_with(x: i32, y: i32, modifiers: Modifiers) -> Event {
    Event::MouseUp {
        x,
        y,
        button: MouseButton::Left,
        modifiers,
    }
}

fn ink(image: &Image, rect: (i32, i32, i32, i32)) -> usize {
    let (l, t, r, b) = rect;
    (t..b)
        .flat_map(|y| (l..r).map(move |x| (x, y)))
        .filter(|&(x, y)| !is(image, x as u32, y as u32, WHITE))
        .count()
}

#[test]
fn esc_during_a_drag_takes_the_stroke_back() {
    let image = session_look(Look::plain(), |stage, observer, parts, _| {
        let areas = &parts.areas;
        stage.emit(Msg::Tool(Tool::Brush));
        stage.emit(Msg::Size(12));
        let (x, y) = areas.canvas_at(100, 100);
        stage.inject(down(x, y, MouseButton::Left));
        move_to(stage, (x + 60, y));
        assert!(observer.borrow().dragging);
        press(stage, Key::ESCAPE);
        assert!(!observer.borrow().dragging, "Esc ended the drag");
        assert!(!observer.borrow().can_undo, "and recorded nothing");
        // The button comes up later; it must not paint or commit anything.
        stage.inject(up(x + 60, y, MouseButton::Left));
        assert!(!observer.borrow().can_undo);
        // A fresh stroke works afterwards.
        drag(stage, (x, y + 30), (x + 40, y + 30), MouseButton::Left);
        assert!(observer.borrow().can_undo);
    });
    let (x, y) = session_areas().canvas_at(130, 100);
    assert!(
        is(&image, x as u32, y as u32, WHITE),
        "the stroke was taken back"
    );
    let (x, y) = session_areas().canvas_at(120, 130);
    assert!(
        is(&image, x as u32, y as u32, BLACK),
        "the next one painted"
    );
}

#[test]
fn shift_snaps_a_line_to_fifteen_degrees_as_it_is_dragged() {
    let image = session_look(Look::plain(), |stage, observer, parts, _| {
        let areas = &parts.areas;
        stage.emit(Msg::Tool(Tool::Line));
        stage.emit(Msg::Size(3));
        let (x, y) = areas.canvas_at(60, 60);
        stage.inject(down_with(x, y, SHIFT));
        stage.inject(move_with(x + 100, y + 6, SHIFT));
        stage.inject(up_with(x + 100, y + 6, SHIFT));
        assert!(observer.borrow().can_undo);
    });
    let areas = session_areas();
    let (x, y) = areas.canvas_at(150, 60);
    assert!(is(&image, x as u32, y as u32, BLACK), "the line is level");
    let (x, y) = areas.canvas_at(150, 65);
    assert!(
        is(&image, x as u32, y as u32, WHITE),
        "it ignored the 3 degrees"
    );
}

#[test]
fn every_shape_and_fill_tool_is_one_undo_step_from_the_window() {
    session_look(Look::plain(), |stage, observer, parts, _| {
        let areas = &parts.areas;
        for tool in [
            Tool::Pencil,
            Tool::Brush,
            Tool::Eraser,
            Tool::Line,
            Tool::Rectangle,
            Tool::RoundedRectangle,
            Tool::Ellipse,
            Tool::Gradient,
            Tool::Fill,
        ] {
            stage.emit(Msg::Tool(tool));
            let before = observer.borrow().history;
            let (x, y) = areas.canvas_at(60, 60);
            stage.inject(down(x, y, MouseButton::Left));
            for step in 1..=4 {
                move_to(stage, (x + step * 20, y + step * 15));
            }
            stage.inject(up(x + 80, y + 60, MouseButton::Left));
            let after = observer.borrow().history;
            assert_eq!(after.0, before.0 + 1, "{tool:?}: one step");
            ctrl(stage, Key::Z);
            assert_eq!(observer.borrow().history.0, before.0, "{tool:?}: undone");
            ctrl(stage, Key::Y);
            ctrl(stage, Key::Z);
        }
    });
}

#[test]
fn the_pencil_with_shift_draws_from_the_last_point() {
    let image = session_look(Look::plain(), |stage, _, parts, _| {
        let areas = &parts.areas;
        let (x, y) = areas.canvas_at(40, 40);
        stage.inject(down(x, y, MouseButton::Left));
        stage.inject(up(x, y, MouseButton::Left));
        stage.inject(down_with(x + 50, y, SHIFT));
        stage.inject(up_with(x + 50, y, SHIFT));
    });
    let (x, y) = session_areas().canvas_at(65, 40);
    assert!(is(&image, x as u32, y as u32, BLACK), "joined by a line");
}

#[test]
fn the_zoom_tool_zooms_in_out_and_to_a_rectangle() {
    session_look(Look::plain(), |stage, observer, parts, _| {
        let areas = &parts.areas;
        press(stage, Key::Z);
        assert_eq!(observer.borrow().tool, Tool::Zoom);
        let (x, y) = areas.canvas_at(160, 120);
        click(stage, (x, y), MouseButton::Left);
        assert_eq!(observer.borrow().zoom_percent, 200, "a click zooms in");
        assert!(!observer.borrow().can_undo, "and is not an edit");
        let (x, y) = {
            let vp = xui_paint::viewport::Viewport::new();
            let _ = vp;
            (areas.canvas.left + 200, areas.canvas.top + 100)
        };
        click(stage, (x, y), MouseButton::Right);
        assert_eq!(
            observer.borrow().zoom_percent,
            100,
            "a right click zooms out"
        );
        // Dragging a small rectangle fills the view with it.
        let (x, y) = areas.canvas_at(100, 80);
        drag(stage, (x, y), (x + 40, y + 30), MouseButton::Left);
        let zoom = observer.borrow().zoom_percent;
        assert!(zoom >= 800, "zoomed to the rectangle: {zoom}");
        // Esc during a Zoom drag cancels it.
        press(stage, Key::DIGIT0);
        ctrl(stage, Key::DIGIT0);
        assert_eq!(observer.borrow().zoom_percent, 100);
        let (x, y) = areas.canvas_at(100, 80);
        stage.inject(down(x, y, MouseButton::Left));
        move_to(stage, (x + 40, y + 30));
        press(stage, Key::ESCAPE);
        stage.inject(up(x + 40, y + 30, MouseButton::Left));
        assert_eq!(
            observer.borrow().zoom_percent,
            100,
            "Esc cancelled the zoom"
        );
    });
}

#[test]
fn the_pan_tool_drags_the_view_and_never_paints() {
    session_look(Look::plain(), |stage, observer, parts, _| {
        let areas = &parts.areas;
        for _ in 0..3 {
            ctrl(stage, Key::from_code(0xBB)); // Ctrl+=
        }
        assert!(observer.borrow().zoom_percent > 200);
        press(stage, Key::H);
        assert_eq!(observer.borrow().tool, Tool::Pan);
        let before = observer.borrow().view_origin;
        let (x, y) = (areas.canvas.left + 200, areas.canvas.top + 150);
        drag(stage, (x, y), (x - 60, y - 40), MouseButton::Left);
        let after = observer.borrow().view_origin;
        assert_ne!(before, after, "the view moved");
        assert_eq!(after.0 - before.0, 60, "by what the pointer moved");
        assert!(!observer.borrow().can_undo, "nothing was drawn");
    });
}

#[test]
fn the_options_bar_shows_each_tools_controls_and_hides_the_rest() {
    session_look(Look::plain(), |stage, _, _, probed| {
        let shown = |name: &str| probed.borrow().contains_key(name);
        stage.emit(Msg::Tool(Tool::Brush));
        probe(stage, probed);
        assert!(shown("size") && shown("hardness") && shown("antialias"));
        assert!(!shown("arrow") && !shown("gradient_kind") && !shown("font_family"));
        stage.emit(Msg::Tool(Tool::Line));
        probe(stage, probed);
        assert!(shown("arrow") && shown("size") && !shown("hardness"));
        stage.emit(Msg::Tool(Tool::RoundedRectangle));
        probe(stage, probed);
        assert!(shown("radius") && shown("shape_mode"));
        stage.emit(Msg::Tool(Tool::Fill));
        probe(stage, probed);
        assert!(shown("fill_tolerance") && shown("fill_area") && shown("fill_sample"));
        assert!(!shown("size"));
        stage.emit(Msg::Tool(Tool::Picker));
        probe(stage, probed);
        assert!(shown("pick_size") && shown("pick_sample") && shown("pick_back"));
        stage.emit(Msg::Tool(Tool::Gradient));
        probe(stage, probed);
        assert!(shown("gradient_kind"));
        stage.emit(Msg::Tool(Tool::Text));
        probe(stage, probed);
        assert!(shown("font_family") && shown("font_size") && shown("bold"));
        assert!(shown("italic") && shown("underline") && shown("text_align"));
        stage.emit(Msg::Tool(Tool::Pencil));
        probe(stage, probed);
        assert!(!shown("size") && !shown("font_family"));
    });
}

#[test]
fn the_antialiasing_check_box_is_bound_to_the_brush() {
    let draw = |clicks: usize| {
        session_look(Look::plain(), move |stage, _, parts, probed| {
            stage.emit(Msg::Tool(Tool::Brush));
            stage.emit(Msg::Size(13));
            probe(stage, probed);
            for _ in 0..clicks {
                click(stage, centre(probed, "antialias"), MouseButton::Left);
            }
            let (x, y) = parts.areas.canvas_at(100, 100);
            drag(stage, (x, y), (x, y), MouseButton::Left);
            // The brush ring under the pointer is antialiased too: put the
            // tool away so only the dab is measured.
            stage.emit(Msg::Tool(Tool::Pan));
        })
    };
    let area = |image: &Image| {
        let (x, y) = common::driver::session_areas().canvas_at(100, 100);
        let (l, t) = (x - 10, y - 10);
        let mut grey = 0;
        for yy in t..t + 21 {
            for xx in l..l + 21 {
                let p = image.pixel(xx as u32, yy as u32).unwrap();
                if p[0] != 255 && p[0] != 0 {
                    grey += 1;
                }
            }
        }
        grey
    };
    assert!(area(&draw(0)) > 0, "antialiased by default");
    assert_eq!(area(&draw(1)), 0, "unchecking it gives a hard edge");
}

#[test]
fn the_picker_switch_back_check_box_returns_to_the_previous_tool() {
    session_look(Look::plain(), |stage, observer, parts, probed| {
        press(stage, Key::B);
        press(stage, Key::K);
        probe(stage, probed);
        click(stage, centre(probed, "pick_back"), MouseButton::Left);
        let (x, y) = parts.areas.canvas_at(10, 10);
        click(stage, (x, y), MouseButton::Left);
        assert_eq!(observer.borrow().tool, Tool::Brush, "back to the brush");
        assert_eq!(observer.borrow().primary, WHITE, "after picking the page");
    });
}

#[test]
fn option_messages_change_what_the_next_shape_looks_like() {
    let image = session_look(Look::plain(), |stage, _, parts, _| {
        stage.emit(Msg::Tool(Tool::Rectangle));
        stage.emit(Msg::Size(4));
        stage.emit(Msg::Opt(OptMsg::ShapeMode(
            xui_paint::model::tools::FillMode::Both,
        )));
        let (x, y) = parts.areas.canvas_at(60, 60);
        drag(stage, (x, y), (x + 100, y + 80), MouseButton::Left);
    });
    let areas = session_areas();
    let (x, y) = areas.canvas_at(110, 100);
    assert!(
        is(&image, x as u32, y as u32, WHITE),
        "filled with the white secondary"
    );
    let (x, y) = areas.canvas_at(110, 61);
    assert!(is(&image, x as u32, y as u32, BLACK), "outlined in black");
    assert!(ink(&image, (x - 20, y - 2, x + 20, y + 3)) > 20);
}

#[cfg(feature = "text")]
mod text {
    use super::*;
    use common::setup::liberation;

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
}
