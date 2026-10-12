//! Selections through the real window: tool-grid clicks, canvas drags with
//! modifiers, shortcuts, menu rows, the oversized-paste dialog, the system
//! clipboard seam and snapshots of the marching ants at 100% and 400%.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::backend::Event;
use xui_core::message::{Key, Modifiers, MouseButton};
use xui_paint::Msg;
use xui_paint::model::Bitmap;
use xui_paint::view::{Command, SystemClipboard, menu_item};

use common::driver::*;

const CTRL: Modifiers = Modifiers {
    ctrl: true,
    ..Modifiers::NONE
};

/// Tool-grid cells of the P2 tools.
const RECT_SELECT: i32 = 8;
const ELLIPSE_SELECT: i32 = 9;
const LASSO: i32 = 10;
const WAND: i32 = 11;

fn drag_with(
    stage: &Stg<'_>,
    from: (i32, i32),
    to: (i32, i32),
    button: MouseButton,
    mods: Modifiers,
) {
    stage.inject(Event::MouseDown {
        x: from.0,
        y: from.1,
        button,
        modifiers: mods,
    });
    stage.inject(Event::MouseMove {
        x: to.0,
        y: to.1,
        modifiers: mods,
    });
    stage.inject(Event::MouseUp {
        x: to.0,
        y: to.1,
        button,
        modifiers: mods,
    });
}

fn selection(observer: &Rc<RefCell<xui_paint::Observer>>) -> Option<(i32, i32, u32, u32)> {
    observer.borrow().selection
}

#[test]
fn the_tool_grid_and_canvas_make_selections_and_the_status_bar_sizes_them() {
    session(|stage, observer, parts| {
        let a = &parts.areas;
        click(stage, tool_cell(a, RECT_SELECT), MouseButton::Left);
        assert_eq!(status(observer, 2), "Rectangle Select");
        drag(
            stage,
            a.canvas_at(20, 20),
            a.canvas_at(99, 79),
            MouseButton::Left,
        );
        assert_eq!(selection(observer), Some((20, 20, 80, 60)));
        assert_eq!(status(observer, 1), "320 x 240  sel 80x60");
        // Ctrl adds a second rectangle; the right button takes one out.
        drag_with(
            stage,
            a.canvas_at(150, 20),
            a.canvas_at(179, 49),
            MouseButton::Left,
            CTRL,
        );
        assert_eq!(selection(observer), Some((20, 20, 160, 60)));
        drag_with(
            stage,
            a.canvas_at(20, 20),
            a.canvas_at(59, 79),
            MouseButton::Right,
            Modifiers::NONE,
        );
        assert_eq!(selection(observer), Some((60, 20, 120, 60)));
        // Ellipse, lasso and wand are on the grid too.
        click(stage, tool_cell(a, ELLIPSE_SELECT), MouseButton::Left);
        drag(
            stage,
            a.canvas_at(0, 0),
            a.canvas_at(49, 49),
            MouseButton::Left,
        );
        assert_eq!(selection(observer), Some((0, 0, 50, 50)));
        click(stage, tool_cell(a, LASSO), MouseButton::Left);
        assert_eq!(status(observer, 2), "Lasso Select");
        click(stage, tool_cell(a, WAND), MouseButton::Left);
        click(stage, a.canvas_at(100, 100), MouseButton::Left);
        assert_eq!(
            selection(observer),
            Some((0, 0, 320, 240)),
            "the blank page is one region"
        );
        ctrl(stage, Key::D);
        assert_eq!(selection(observer), None);
        assert_eq!(status(observer, 1), "320 x 240");
        ctrl(stage, Key::Z);
        assert!(selection(observer).is_some(), "Deselect is an undo step");
    });
}

#[test]
fn s_and_m_cycle_their_tools_and_select_keys_edit_the_selection() {
    session(|stage, observer, _| {
        let tool = || observer.borrow().tool;
        for expected in [
            xui_paint::model::Tool::RectSelect,
            xui_paint::model::Tool::EllipseSelect,
            xui_paint::model::Tool::Lasso,
            xui_paint::model::Tool::MagicWand,
            xui_paint::model::Tool::RectSelect,
        ] {
            press(stage, Key::S);
            assert_eq!(tool(), expected);
        }
        press(stage, Key::M);
        assert_eq!(tool(), xui_paint::model::Tool::MovePixels);
        press(stage, Key::M);
        assert_eq!(tool(), xui_paint::model::Tool::MoveSelection);
        ctrl(stage, Key::A);
        assert_eq!(selection(observer), Some((0, 0, 320, 240)));
        assert_eq!(status(observer, 1), "320 x 240  sel 320x240");
        ctrl(stage, Key::I);
        assert_eq!(selection(observer), None, "the inverse of everything");
        ctrl(stage, Key::A);
        ctrl(stage, Key::D);
        assert_eq!(selection(observer), None);
    });
}

#[test]
fn backspace_fills_and_delete_erases_the_selection() {
    let image = session(|stage, observer, parts| {
        let a = &parts.areas;
        click(stage, tool_cell(a, RECT_SELECT), MouseButton::Left);
        drag(
            stage,
            a.canvas_at(10, 10),
            a.canvas_at(59, 59),
            MouseButton::Left,
        );
        press(stage, Key::BACK);
        assert_eq!(observer.borrow().status[3], "");
        drag(
            stage,
            a.canvas_at(100, 10),
            a.canvas_at(159, 59),
            MouseButton::Left,
        );
        press(stage, Key::BACK);
        press(stage, Key::DELETE);
        // Pencil strokes stay inside the current selection.
        click(stage, tool_cell(a, 0), MouseButton::Left);
        drag(
            stage,
            a.canvas_at(90, 30),
            a.canvas_at(190, 30),
            MouseButton::Left,
        );
    });
    let areas = session_areas();
    let at = |x, y| {
        let (px, py) = areas.canvas_at(x, y);
        (px as u32, py as u32)
    };
    let (x, y) = at(30, 30);
    assert!(is(&image, x, y, BLACK), "filled with the primary colour");
    let (x, y) = at(130, 40);
    assert!(
        !is(&image, x, y, BLACK),
        "erased area is transparent, not black"
    );
    let (x, y) = at(120, 30);
    assert!(
        is(&image, x, y, BLACK),
        "the pencil drew inside the selection"
    );
    let (x, y) = at(80, 30);
    assert!(is(&image, x, y, WHITE), "and nowhere outside it");
    let (x, y) = at(170, 30);
    assert!(
        !is(&image, x, y, BLACK),
        "even past the selection's right edge"
    );
}

#[test]
fn copy_paste_float_nudge_land_and_cancel() {
    session(|stage, observer, parts| {
        let a = &parts.areas;
        let cut = menu_item(Msg::Command(Command::Cut)).unwrap();
        let paste = menu_item(Msg::Command(Command::Paste)).unwrap();
        assert!(!parts.menu.is_enabled(cut) && !parts.menu.is_enabled(paste));
        click(stage, tool_cell(a, RECT_SELECT), MouseButton::Left);
        drag(
            stage,
            a.canvas_at(10, 10),
            a.canvas_at(49, 39),
            MouseButton::Left,
        );
        assert!(parts.menu.is_enabled(cut), "a selection enables Cut");
        assert!(!parts.menu.is_enabled(paste), "nothing copied yet");
        ctrl(stage, Key::C);
        assert_eq!(status(observer, 3), "Copied");
        assert!(parts.menu.is_enabled(paste));
        // Paste lands at the top-left of the view, floating, with the move tool.
        ctrl(stage, Key::V);
        assert!(observer.borrow().floating);
        assert_eq!(observer.borrow().tool, xui_paint::model::Tool::MovePixels);
        assert_eq!(selection(observer), Some((0, 0, 40, 30)));
        let steps = observer.borrow().can_undo;
        // Arrows nudge by one, Shift by ten.
        press(stage, Key::RIGHT);
        press_with(
            stage,
            Key::DOWN,
            Modifiers {
                shift: true,
                ..Modifiers::NONE
            },
        );
        assert_eq!(selection(observer), Some((1, 10, 40, 30)));
        press(stage, Key::RETURN);
        assert!(!observer.borrow().floating, "Enter lands it");
        assert!(steps);
        ctrl(stage, Key::Z);
        assert_eq!(
            selection(observer),
            Some((10, 10, 40, 30)),
            "undo takes the paste back whole"
        );
        ctrl(stage, Key::Y);
        // Esc drops a fresh paste without a trace.
        ctrl(stage, Key::D);
        ctrl(stage, Key::V);
        assert!(observer.borrow().floating);
        press(stage, Key::ESCAPE);
        assert!(!observer.borrow().floating);
        assert_eq!(selection(observer), None);
        // Dragging the floating pixels with the mouse and a click outside landing them.
        ctrl(stage, Key::V);
        drag(
            stage,
            a.canvas_at(5, 5),
            a.canvas_at(75, 55),
            MouseButton::Left,
        );
        assert_eq!(selection(observer), Some((70, 50, 40, 30)));
        click(stage, a.canvas_at(300, 200), MouseButton::Left);
        assert!(!observer.borrow().floating);
    });
}

#[test]
fn escape_and_arrows_are_free_when_no_move_tool_is_active() {
    session(|stage, observer, _| {
        // Esc on the plain pencil does nothing, and the arrows do not claim keys.
        press(stage, Key::ESCAPE);
        press(stage, Key::LEFT);
        assert!(!observer.borrow().floating);
        assert_eq!(selection(observer), None);
    });
}

/// A system clipboard that keeps what it is given in memory.
struct Fake {
    max: usize,
    offered: RefCell<Option<Vec<u8>>>,
    foreign: RefCell<Option<Vec<u8>>>,
}

impl Fake {
    fn new(max: usize) -> Rc<Fake> {
        Rc::new(Fake {
            max,
            offered: RefCell::new(None),
            foreign: RefCell::new(None),
        })
    }
}

impl SystemClipboard for Fake {
    fn max_bytes(&self) -> usize {
        self.max
    }
    fn offer_png(&self, png: &[u8]) {
        *self.offered.borrow_mut() = Some(png.to_vec());
    }
    fn request_png(&self) -> Option<Vec<u8>> {
        self.offered
            .borrow()
            .clone()
            .or_else(|| self.foreign.borrow().clone())
    }
}

#[test]
fn a_small_copy_is_offered_as_png_and_a_big_one_stays_in_the_app() {
    let small = Fake::new(8 * 1024);
    let probe = Rc::clone(&small);
    session_with(Some(small), move |stage, observer, parts| {
        let a = &parts.areas;
        click(stage, tool_cell(a, RECT_SELECT), MouseButton::Left);
        drag(
            stage,
            a.canvas_at(0, 0),
            a.canvas_at(19, 9),
            MouseButton::Left,
        );
        ctrl(stage, Key::C);
        let png = probe.offered.borrow().clone().expect("offered");
        let decoded = Bitmap::decode(&png).unwrap();
        assert_eq!(decoded.size(), (20, 10));
        assert_eq!(status(observer, 3), "Copied");
    });

    // A cap too small for any picture: nothing is offered, and a picture
    // another application copied earlier does not win over our newer copy.
    let tiny = Fake::new(10);
    *tiny.foreign.borrow_mut() = Some(Bitmap::new(40, 40, RED).encode_png().unwrap());
    let probe = Rc::clone(&tiny);
    session_with(Some(tiny), move |stage, observer, parts| {
        let a = &parts.areas;
        click(stage, tool_cell(a, RECT_SELECT), MouseButton::Left);
        drag(
            stage,
            a.canvas_at(0, 0),
            a.canvas_at(19, 9),
            MouseButton::Left,
        );
        ctrl(stage, Key::C);
        assert!(probe.offered.borrow().is_none(), "too big to offer");
        ctrl(stage, Key::V);
        assert_eq!(
            selection(observer),
            Some((0, 0, 20, 10)),
            "our own copy, not the foreign one"
        );
    });
}

#[test]
fn a_picture_from_another_app_pastes_and_an_oversized_one_asks_first() {
    let sys = Fake::new(8 * 1024);
    *sys.foreign.borrow_mut() = Some(Bitmap::new(400, 300, RED).encode_png().unwrap());
    let paste = sys.clone();
    session_with(Some(sys), move |stage, observer, parts| {
        let _ = &paste;
        // 400 x 300 does not fit 320 x 240: Esc keeps the canvas.
        ctrl(stage, Key::V);
        assert!(!observer.borrow().floating, "the dialog is waiting");
        press(stage, Key::ESCAPE);
        assert!(observer.borrow().floating);
        assert_eq!(status(observer, 1), "320 x 240  sel 320x240");
        press(stage, Key::ESCAPE);
        assert!(!observer.borrow().floating);
        // Enter expands the canvas to fit.
        ctrl(stage, Key::V);
        press(stage, Key::RETURN);
        assert!(observer.borrow().floating);
        assert_eq!(status(observer, 1), "400 x 300  sel 400x300");
        let _ = parts;
    });
}

#[test]
fn paste_into_new_layer_and_new_image_through_the_menu_keys() {
    let sys = Fake::new(8 * 1024);
    *sys.foreign.borrow_mut() = Some(Bitmap::new(30, 20, RED).encode_png().unwrap());
    session_with(Some(sys), |stage, observer, _| {
        stage.emit(Msg::Command(Command::PasteNewLayer));
        assert!(observer.borrow().floating);
        press(stage, Key::RETURN);
        ctrl(stage, Key::Z);
        assert!(!observer.borrow().floating);
        stage.emit(Msg::Command(Command::PasteNewImage));
        assert_eq!(status(observer, 1), "30 x 20");
        assert!(
            !observer.borrow().can_undo,
            "a new image starts a new history"
        );
    });
}

#[test]
fn crop_to_selection_resizes_the_picture() {
    session(|stage, observer, parts| {
        let a = &parts.areas;
        let crop = menu_item(Msg::Command(Command::CropToSelection)).unwrap();
        assert!(!parts.menu.is_enabled(crop));
        click(stage, tool_cell(a, RECT_SELECT), MouseButton::Left);
        drag(
            stage,
            a.canvas_at(10, 10),
            a.canvas_at(69, 49),
            MouseButton::Left,
        );
        assert!(parts.menu.is_enabled(crop));
        ctrl_shift(stage, Key::X);
        assert_eq!(status(observer, 1), "60 x 40");
        assert_eq!(selection(observer), None);
        ctrl(stage, Key::Z);
        assert_eq!(status(observer, 1), "320 x 240  sel 60x40");
    });
}

#[test]
fn the_options_bar_shows_the_mode_and_the_wand_controls() {
    session(|stage, observer, parts| {
        let a = &parts.areas;
        click(stage, tool_cell(a, RECT_SELECT), MouseButton::Left);
        stage.emit(Msg::SelectMode(xui_paint::model::CombineMode::Union));
        drag(
            stage,
            a.canvas_at(0, 0),
            a.canvas_at(9, 9),
            MouseButton::Left,
        );
        drag(
            stage,
            a.canvas_at(20, 0),
            a.canvas_at(29, 9),
            MouseButton::Left,
        );
        assert_eq!(
            selection(observer),
            Some((0, 0, 30, 10)),
            "the bar's Union mode joins them"
        );
        stage.emit(Msg::WandTolerance(0));
        stage.emit(Msg::WandContiguous(false));
        click(stage, tool_cell(a, WAND), MouseButton::Left);
        click(stage, a.canvas_at(100, 100), MouseButton::Left);
        assert!(selection(observer).is_some());
    });
}

/// Renders a black page with a rectangle and an ellipse selection and saves it.
fn ants_at(zoom_in: usize, name: &str) {
    let image = session(move |stage, _, parts| {
        let a = &parts.areas;
        ctrl(stage, Key::A);
        press(stage, Key::BACK);
        for _ in 0..zoom_in {
            stage.emit(Msg::Command(Command::ZoomIn));
        }
        // The picture is centred or scrolled; click through the real mapping.
        click(stage, tool_cell(a, RECT_SELECT), MouseButton::Left);
        let (cx, cy) = (
            a.canvas.left + a.canvas.width() / 2,
            a.canvas.top + a.canvas.height() / 2,
        );
        drag(
            stage,
            (cx - 60, cy - 40),
            (cx + 20, cy + 30),
            MouseButton::Left,
        );
        click(stage, tool_cell(a, ELLIPSE_SELECT), MouseButton::Left);
        drag_with(
            stage,
            (cx - 20, cy - 20),
            (cx + 70, cy + 50),
            MouseButton::Left,
            CTRL,
        );
    });
    std::fs::create_dir_all("target/snapshots").unwrap();
    image
        .save_png(format!("target/snapshots/{name}.png"))
        .unwrap();
    // The dashes are white on the black page: count the light pixels there.
    let areas = session_areas();
    let mut white = 0;
    for y in areas.canvas.top + 2..areas.canvas.bottom - 2 {
        for x in areas.canvas.left + 2..areas.canvas.right - 2 {
            // Anti-aliased one-pixel lines are light grey at worst.
            if image
                .pixel(x as u32, y as u32)
                .is_some_and(|p| p[..3].iter().all(|c| *c > 150))
            {
                white += 1;
            }
        }
    }
    assert!(
        white > 10,
        "{name}: {white} white ant pixels on the black page"
    );
}

#[test]
fn the_ants_draw_at_100_and_400_percent() {
    ants_at(0, "paint-ants-100");
    ants_at(3, "paint-ants-400");
}
