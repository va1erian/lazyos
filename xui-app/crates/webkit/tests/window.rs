//! The WebKit window driven offscreen against a recording engine: the engine's
//! news reaches the chrome, and the pointer, wheel and keyboard reach the
//! engine as WebKit wants them. Snapshots go to `target/snapshots/webkit-*.png`
//! for a human to look at.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::backend::Event;
use xui_core::{Dip, Image, Key, Modifiers, MouseButton, Theme};
use xui_webkit::app::{Browser, Host, Msg};
use xui_webkit::engine::{Button, Call, MouseKind, Recorder};
use xui_webkit::mailbox::{Mailbox, PageState};

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

fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

/// A page-sized picture: white with a red block and a blue block.
fn test_page(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = vec![255u8; (width * height * 4) as usize];
    let mut fill = |x0: u32, y0: u32, x1: u32, y1: u32, rgb: [u8; 3]| {
        for y in y0..y1.min(height) {
            for x in x0..x1.min(width) {
                let at = ((y * width + x) * 4) as usize;
                rgba[at..at + 3].copy_from_slice(&rgb);
            }
        }
    };
    fill(20, 20, 220, 80, [220, 30, 30]);
    fill(20, 100, 220, 160, [30, 60, 220]);
    rgba
}

struct Rig {
    engine: Rc<Recorder>,
    mailbox: Arc<Mailbox>,
}

fn drive(
    first: Option<&str>,
    step: impl FnOnce(&Stage<'_, Msg>, &Rig) + 'static,
) -> (Image, Vec<Call>) {
    register_fonts();
    let engine = Rc::new(Recorder::default());
    let mailbox = Arc::new(Mailbox::default());
    let rig = Rig {
        engine: Rc::clone(&engine),
        mailbox: Arc::clone(&mailbox),
    };
    let first = first.map(str::to_owned);
    let (build_engine, build_mailbox) = (Rc::clone(&engine), Arc::clone(&mailbox));
    let image = render_with(
        Snapshot::new(Dip(SIZE.0 as f32), Dip(SIZE.1 as f32)).theme(Theme::light()),
        move |ui| Browser::build(ui, build_engine, build_mailbox, first, Host::none()),
        move |stage| step(stage, &rig),
    )
    .expect("the headless render");
    (image, engine.calls())
}

/// The page view's rectangle in the window: below the toolbar.
fn page_origin(stage: &Stage<'_, Msg>) -> (i32, i32) {
    let dpi = stage.dpi();
    (0, Dip(38.0).to_px(dpi).0)
}

#[test]
fn the_first_address_is_loaded_and_the_page_is_laid_out_at_the_view_size() {
    let (_, calls) = drive(Some("http://example.com/"), |_, _| {});
    assert!(
        calls.contains(&Call::Load("http://example.com/".into())),
        "{calls:?}"
    );
    let resized: Vec<_> = calls
        .iter()
        .filter_map(|c| match c {
            Call::Resize(w, h) => Some((*w, *h)),
            _ => None,
        })
        .collect();
    let (w, h) = *resized.last().expect("the engine was told the view size");
    assert_eq!(w, SIZE.0);
    assert!(h > 300 && h < SIZE.1, "view height {h}");
}

#[test]
fn a_frame_from_the_engine_is_drawn_in_the_page_area() {
    let (image, _) = drive(None, |stage, rig| {
        rig.mailbox.post_frame(780, 400, &test_page(780, 400));
        stage.emit(Msg::Frame);
        rig.mailbox.post_state(PageState {
            url: "http://example.com/".into(),
            title: "Example Domain".into(),
            can_back: true,
            ..PageState::default()
        });
        stage.emit(Msg::State);
    });
    save(&image, "webkit-frame.png");
    let reds = image
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > 200 && p[1] < 60 && p[2] < 60)
        .count();
    let blues = image
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[2] > 200 && p[0] < 60 && p[1] < 100)
        .count();
    assert!(reds > 5_000, "the red block is on screen: {reds}");
    assert!(blues > 5_000, "the blue block is on screen: {blues}");
}

fn red_pixels(image: &Image) -> usize {
    image
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > 200 && p[1] < 60 && p[2] < 60)
        .count()
}

/// A page on show, then the engine's news that its load failed or its process went away.
fn after_a_failure(state: PageState) -> Image {
    let (image, _) = drive(None, move |stage, rig| {
        rig.mailbox.post_frame(780, 400, &test_page(780, 400));
        stage.emit(Msg::Frame);
        rig.mailbox.post_state(PageState {
            url: "http://example.com/".into(),
            title: "Example Domain".into(),
            ..PageState::default()
        });
        stage.emit(Msg::State);
        rig.mailbox.post_state(state);
        stage.emit(Msg::State);
        // A picture painted after the process died must not bring the page back.
        rig.mailbox.post_frame(780, 400, &test_page(780, 400));
        stage.emit(Msg::Frame);
    });
    image
}

#[test]
fn a_failed_load_keeps_the_page_on_show_and_a_dead_process_blanks_it() {
    let failed = after_a_failure(PageState {
        url: "http://example.com:81/".into(),
        error: "Could not load the page: Connection refused".into(),
        ..PageState::default()
    });
    save(&failed, "webkit-load-failed.png");
    assert!(
        red_pixels(&failed) > 5_000,
        "the page stays after a failed load"
    );

    let crashed = after_a_failure(PageState {
        url: "http://example.com/".into(),
        error: "The page's process stopped. Reload (F5) starts it again.".into(),
        crashed: true,
        ..PageState::default()
    });
    save(&crashed, "webkit-crashed.png");
    assert_eq!(red_pixels(&crashed), 0, "a crashed page is blank");
}

/// The toolbar's pixels after the page said it is at `http://two/`, with the address field
/// half-typed (`typed`) and the page clicked (`clicked`) or not.
fn toolbar_after(typed: bool, clicked: bool) -> Vec<u8> {
    let (image, _) = drive(None, move |stage, rig| {
        rig.mailbox.post_state(PageState {
            url: "http://one/".into(),
            ..PageState::default()
        });
        stage.emit(Msg::State);
        if typed {
            stage.emit(Msg::AddressEdited);
        }
        if clicked {
            let (ox, oy) = page_origin(stage);
            stage.click(ox + 50, oy + 40);
        }
        rig.mailbox.post_state(PageState {
            url: "http://two/".into(),
            ..PageState::default()
        });
        stage.emit(Msg::State);
    });
    let row = image.width() as usize * 4;
    image.pixels()[..row * 38].to_vec()
}

#[test]
fn clicking_the_page_ends_address_editing_so_its_own_address_shows_again() {
    let untouched = toolbar_after(false, false);
    // Half-typed: the page's news does not overwrite what the user is typing.
    assert_ne!(toolbar_after(true, false), untouched);
    // A click in the page abandons the edit: the next address shows.
    assert_eq!(toolbar_after(true, true), untouched);
}

#[test]
fn clicks_moves_and_wheel_reach_the_engine_in_page_coordinates() {
    let (_, calls) = drive(None, |stage, rig| {
        let (ox, oy) = page_origin(stage);
        rig.engine.take();
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
    let (_, calls) = drive(None, |stage, rig| {
        let (ox, oy) = page_origin(stage);
        stage.click(ox + 50, oy + 40);
        rig.engine.take();
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
fn the_windows_own_shortcuts_do_not_reach_the_page() {
    let (_, calls) = drive(None, |stage, rig| {
        let (ox, oy) = page_origin(stage);
        stage.click(ox + 50, oy + 40);
        rig.engine.take();
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        for key in [Key::L, Key::R, Key::F5] {
            stage.inject(Event::KeyDown {
                key,
                modifiers: ctrl,
                repeat: 1,
                system: false,
            });
            stage.inject(Event::KeyUp {
                key,
                modifiers: ctrl,
                system: false,
            });
        }
    });
    let keys: Vec<_> = calls
        .iter()
        .filter(|c| matches!(c, Call::Key(..)))
        .collect();
    assert!(keys.is_empty(), "the page got the window's keys: {keys:?}");
}

#[test]
fn the_address_field_navigates_and_the_buttons_follow_the_page() {
    let (image, calls) = drive(None, |stage, rig| {
        stage.click(300, 19);
        for c in "example.com".chars() {
            stage.inject(Event::Char(c));
        }
        stage.emit(Msg::Go);
        rig.mailbox.post_state(PageState {
            url: "http://example.com/".into(),
            title: "Example Domain".into(),
            loading: true,
            progress: 420,
            ..PageState::default()
        });
        stage.emit(Msg::State);
        stage.emit(Msg::ReloadOrStop);
        rig.mailbox.post_state(PageState {
            url: "http://example.com/".into(),
            title: "Example Domain".into(),
            can_back: true,
            ..PageState::default()
        });
        stage.emit(Msg::State);
        stage.emit(Msg::ReloadOrStop);
        stage.emit(Msg::Back);
        stage.emit(Msg::Forward);
    });
    save(&image, "webkit-chrome.png");
    let wanted = [
        Call::Load("http://example.com/".into()),
        Call::Stop,
        Call::Reload,
        Call::Back,
        Call::Forward,
    ];
    let seen: Vec<_> = calls
        .iter()
        .filter(|c| !matches!(c, Call::Resize(..) | Call::Focus(_)))
        .cloned()
        .collect();
    assert_eq!(seen, wanted, "{calls:?}");
}

#[test]
fn a_right_button_drag_keeps_the_capture_until_it_is_released() {
    let (_, calls) = drive(None, |stage, rig| {
        let (ox, oy) = page_origin(stage);
        rig.engine.take();
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
