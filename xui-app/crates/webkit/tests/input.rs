//! The page view's input, driven offscreen against a recording engine: the pointer, the wheel and the
//! keyboard reach the engine as WebKit wants them, in page coordinates (what the W5 window's tests
//! used to cover through its chrome; a window's own shortcuts are that window's tests).

use std::rc::Rc;
use std::sync::Arc;

use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::backend::Event;
use xui_core::{Dip, Key, Modifiers, MouseButton, Theme};
use xui_webkit::engine::{Button, Call, MouseKind, Recorder};
use xui_webkit::events::CursorKind;
use xui_webkit::mailbox::Mailbox;

mod support;
use support::{Browser, Host, Msg, STRIP};

const SIZE: (i32, i32) = (800, 500);

fn register_fonts() {
    let fonts: [&[u8]; 2] = [
        include_bytes!("../../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../../assets/fonts/DroidSans-Bold.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
    xui_canvas::set_default_family("Droid Sans");
}

/// Runs `step` against a view with an empty recording engine and returns what the engine was told.
fn drive(step: impl FnOnce(&Stage<'_, Msg>, &Recorder) + 'static) -> Vec<Call> {
    register_fonts();
    let engine = Rc::new(Recorder::default());
    let mailbox = Arc::new(Mailbox::default());
    let (build_engine, build_mailbox, drive_engine) =
        (Rc::clone(&engine), Arc::clone(&mailbox), Rc::clone(&engine));
    render_with(
        Snapshot::new(Dip(SIZE.0 as f32), Dip(SIZE.1 as f32)).theme(Theme::light()),
        move |ui| Browser::build(ui, build_engine, build_mailbox, None, Host::none()),
        move |stage| step(stage, &drive_engine),
    )
    .expect("the headless render");
    engine.calls()
}

/// The view's top-left corner in the window: below the strip.
fn page_origin(stage: &Stage<'_, Msg>) -> (i32, i32) {
    (0, Dip(STRIP).to_px(stage.dpi()).0)
}

#[test]
fn clicks_and_wheel_reach_the_engine_in_page_coordinates() {
    let calls = drive(|stage, engine| {
        let (ox, oy) = page_origin(stage);
        engine.take();
        stage.click(ox + 50, oy + 40);
        stage.inject(Event::MouseWheel {
            delta: -120,
            horizontal: false,
            x: ox + 60,
            y: oy + 70,
            modifiers: Modifiers::NONE,
        });
    });
    let mouse: Vec<_> = calls
        .iter()
        .filter_map(|c| match c {
            Call::Mouse(kind, button, x, y, _) => Some((*kind, *button, *x, *y)),
            _ => None,
        })
        .collect();
    assert_eq!(
        mouse,
        vec![
            (MouseKind::Down, Button::Left, 50, 40),
            (MouseKind::Up, Button::Left, 50, 40)
        ],
        "{calls:?}"
    );
    assert!(
        calls.contains(&Call::Wheel(60, 70, 0, -1, 0)),
        "a notch down is one step down: {calls:?}"
    );
}

#[test]
fn typing_in_the_page_sends_text_and_the_keys_that_type_nothing() {
    let calls = drive(|stage, engine| {
        let (ox, oy) = page_origin(stage);
        stage.click(ox + 50, oy + 40);
        engine.take();
        stage.inject(Event::Char('h'));
        stage.inject(Event::Char('é'));
        stage.inject(Event::KeyDown {
            key: Key::BACK,
            modifiers: Modifiers::NONE,
            repeat: 1,
            system: false,
        });
        stage.inject(Event::KeyUp {
            key: Key::BACK,
            modifiers: Modifiers::NONE,
            system: false,
        });
        stage.inject(Event::KeyDown {
            key: Key::RETURN,
            modifiers: Modifiers::NONE,
            repeat: 1,
            system: false,
        });
    });
    let keys: Vec<_> = calls
        .iter()
        .filter(|c| matches!(c, Call::Key(..)))
        .cloned()
        .collect();
    assert_eq!(
        keys,
        vec![
            Call::Key(true, 0x68, "h".into(), 0),
            Call::Key(false, 0x68, String::new(), 0),
            Call::Key(true, 0xe9, "é".into(), 0),
            Call::Key(false, 0xe9, String::new(), 0),
            Call::Key(true, 0xff08, String::new(), 0),
            Call::Key(false, 0xff08, String::new(), 0),
            Call::Key(true, 0xff0d, String::new(), 0),
        ],
        "{calls:?}"
    );
}

#[test]
fn a_right_button_drag_keeps_the_capture_until_it_is_released() {
    let calls = drive(|stage, engine| {
        let (ox, oy) = page_origin(stage);
        engine.take();
        stage.inject(Event::MouseDown {
            x: ox + 10,
            y: oy + 10,
            button: MouseButton::Right,
            modifiers: Modifiers::NONE,
        });
        stage.hover(ox + 30, oy + 30);
        stage.inject(Event::MouseUp {
            x: ox + 30,
            y: oy + 30,
            button: MouseButton::Right,
            modifiers: Modifiers::NONE,
        });
    });
    assert!(
        calls.contains(&Call::Mouse(MouseKind::Move, Button::Right, 30, 30, 0)),
        "a move during the drag names the held button: {calls:?}"
    );
}

#[test]
fn a_lost_release_of_a_swallowed_right_press_does_not_eat_a_later_release() {
    let calls = drive(|stage, engine| {
        let (ox, oy) = page_origin(stage);
        let right = |down: bool| {
            let (x, y, modifiers) = (ox + 10, oy + 10, Modifiers::NONE);
            let button = MouseButton::Right;
            if down {
                Event::MouseDown {
                    x,
                    y,
                    button,
                    modifiers,
                }
            } else {
                Event::MouseUp {
                    x,
                    y,
                    button,
                    modifiers,
                }
            }
        };
        // The menu took a press, and its release never arrived.
        support::swallow_right(true);
        stage.inject(right(true));
        // The next press goes to the page, and so must its release.
        support::swallow_right(false);
        engine.take();
        stage.inject(right(true));
        stage.inject(right(false));
    });
    assert!(
        calls.contains(&Call::Mouse(MouseKind::Up, Button::Right, 10, 10, 0)),
        "{calls:?}"
    );
}

fn middle(stage: &Stage<'_, Msg>, press: bool, x: i32, y: i32) {
    let (button, modifiers) = (MouseButton::Middle, Modifiers::NONE);
    stage.inject(if press {
        Event::MouseDown {
            x,
            y,
            button,
            modifiers,
        }
    } else {
        Event::MouseUp {
            x,
            y,
            button,
            modifiers,
        }
    });
}

fn mouse_calls(calls: &[Call]) -> Vec<&Call> {
    calls
        .iter()
        .filter(|c| matches!(c, Call::Mouse(..)))
        .collect()
}

#[test]
fn a_middle_click_on_bare_page_is_autoscroll_and_never_the_pages() {
    let calls = drive(|stage, engine| {
        let (ox, oy) = page_origin(stage);
        engine.take();
        middle(stage, true, ox + 50, oy + 40);
        middle(stage, false, ox + 50, oy + 40);
        // Moves go to the autoscroll, not the page.
        stage.inject(Event::MouseMove {
            x: ox + 50,
            y: oy + 200,
            modifiers: Modifiers::NONE,
        });
        // A second click ends it, and is not the page's either.
        stage.click(ox + 50, oy + 200);
        stage.inject(Event::MouseMove {
            x: ox + 60,
            y: oy + 210,
            modifiers: Modifiers::NONE,
        });
    });
    let mouse = mouse_calls(&calls);
    assert_eq!(
        mouse,
        vec![&Call::Mouse(MouseKind::Move, Button::None, 60, 210, 0)],
        "only the move after the autoscroll ended: {calls:?}"
    );
}

#[test]
fn dragging_away_and_letting_go_ends_the_autoscroll() {
    let calls = drive(|stage, engine| {
        let (ox, oy) = page_origin(stage);
        engine.take();
        middle(stage, true, ox + 50, oy + 40);
        stage.inject(Event::MouseMove {
            x: ox + 50,
            y: oy + 300,
            modifiers: Modifiers::NONE,
        });
        middle(stage, false, ox + 50, oy + 300);
        stage.inject(Event::MouseMove {
            x: ox + 60,
            y: oy + 310,
            modifiers: Modifiers::NONE,
        });
    });
    assert_eq!(
        mouse_calls(&calls),
        vec![&Call::Mouse(MouseKind::Move, Button::None, 60, 310, 0)],
        "{calls:?}"
    );
}

#[test]
fn a_middle_click_on_a_link_is_the_pages() {
    let calls = drive(|stage, engine| {
        let (ox, oy) = page_origin(stage);
        stage.emit(Msg::Cursor(CursorKind::Hand));
        engine.take();
        middle(stage, true, ox + 50, oy + 40);
        middle(stage, false, ox + 50, oy + 40);
    });
    assert_eq!(
        mouse_calls(&calls),
        vec![
            &Call::Mouse(MouseKind::Down, Button::Middle, 50, 40, 0),
            &Call::Mouse(MouseKind::Up, Button::Middle, 50, 40, 0)
        ],
        "{calls:?}"
    );
}
