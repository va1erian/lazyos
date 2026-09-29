//! Tests for the snapshot API. None of them enters a message loop that could
//! wait: the offscreen backend's loop returns at once, so a bug fails a test
//! instead of hanging it.

use std::cell::Cell;
use std::rc::Rc;

use xui_core::app::{App, Ui};
use xui_core::backend::BackendError;
use xui_core::widget::{Button, CheckBox, Dialog, Edit, HasText, Label};
use xui_core::{Dip, Rect, Theme};

use super::session::capture_on;
use super::{Snapshot, SnapshotError, render, render_with, try_render};
use crate::OffscreenBackend;

enum Msg {
    Text(&'static str),
    Toggled,
    Clicked,
}

/// Keeps the widgets alive and records what reached `update`.
struct Demo {
    label: Label<Msg>,
    seen: Rc<Cell<u32>>,
    _widgets: (Button<Msg>, CheckBox<Msg>, Edit<Msg>),
}

impl App for Demo {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, _ui: &mut Ui<Msg>) {
        self.seen.set(self.seen.get() + 1);
        match msg {
            Msg::Text(text) => self.label.set_text(text),
            Msg::Toggled | Msg::Clicked => {}
        }
    }
}

/// A content button with an optional modal dialog over it.
struct Modal {
    seen: Rc<Cell<u32>>,
    _button: Button<Msg>,
    _dialog: Dialog<Msg>,
}

impl App for Modal {
    type Msg = Msg;

    fn update(&mut self, msg: Msg, _ui: &mut Ui<Msg>) {
        if let Msg::Clicked = msg {
            self.seen.set(self.seen.get() + 1);
        }
    }
}

/// A button that fills the top of a 320x200 window, with the dialog centred
/// over it and clear of the pixels the scrim tests sample.
fn build_modal(ui: &mut Ui<Msg>, seen: Rc<Cell<u32>>, open: bool) -> Result<Modal, BackendError> {
    let button =
        Button::new(ui, Rect::new(0, 0, 320, 96), "Behind")?.on_click(|| Some(Msg::Clicked));
    let dialog = Dialog::message(ui, "Saved", "Your changes were saved.")?;
    if open {
        dialog.open();
    }
    Ok(Modal {
        seen,
        _button: button,
        _dialog: dialog,
    })
}

/// Sums the RGB channels, a cheap relative brightness for a dark/light test.
fn brightness(pixel: [u8; 4]) -> u32 {
    pixel[0] as u32 + pixel[1] as u32 + pixel[2] as u32
}

fn build(ui: &mut Ui<Msg>, seen: Rc<Cell<u32>>) -> Result<Demo, BackendError> {
    let button = Button::new(ui, Rect::new(20, 20, 160, 52), "Push")?;
    let check =
        CheckBox::new(ui, Rect::new(20, 64, 200, 92), "Enabled")?.on_toggle(|_| Some(Msg::Toggled));
    let edit = Edit::new(ui, Rect::new(20, 104, 300, 136), "hello")?;
    edit.focus();
    let label = Label::new(ui, Rect::new(20, 148, 300, 176), "start")?;
    Ok(Demo {
        label,
        seen,
        _widgets: (button, check, edit),
    })
}

fn snapshot() -> Snapshot {
    Snapshot::new(Dip(320.0), Dip(200.0))
}

fn shot(snapshot: Snapshot) -> xui_core::image::Image {
    try_render(snapshot, |ui| build(ui, Rc::default())).expect("a snapshot")
}

#[test]
fn two_renders_of_the_same_app_are_byte_identical() {
    let first = shot(snapshot());
    let second = shot(snapshot());
    assert_eq!(first.pixels(), second.pixels());
    assert_eq!(first.encode_png().unwrap(), second.encode_png().unwrap());
}

#[test]
fn size_theme_and_dpi_are_applied() {
    let light = shot(snapshot());
    let dark = shot(snapshot().theme(Theme::dark()));
    assert_eq!(light.size(), (320, 200));
    assert_ne!(light.pixel(300, 190), dark.pixel(300, 190), "theme differs");
    let big = shot(snapshot().dpi(192));
    assert_eq!(big.size(), (640, 400));
}

#[test]
fn a_render_does_not_leak_its_theme_or_dpi_into_the_next() {
    let before = shot(snapshot());
    let _ = shot(snapshot().theme(Theme::dark()).dpi(192));
    let after = shot(snapshot());
    assert_eq!(before, after);
}

#[test]
fn the_window_is_closed_after_success_and_after_a_failed_build() {
    let backend = Rc::new(OffscreenBackend::with_dpi(96));
    capture_on(&backend, &snapshot(), |ui| build(ui, Rc::default()), |_| {}).unwrap();
    assert_eq!(backend.window_count(), 0);

    let failed = capture_on(
        &backend,
        &snapshot(),
        |_ui: &mut Ui<Msg>| -> Result<Demo, BackendError> { Err(BackendError::CreateFailed("x")) },
        |_| {},
    );
    assert!(matches!(failed, Err(SnapshotError::Backend(_))));
    assert_eq!(backend.window_count(), 0);
}

#[test]
fn bad_sizes_are_errors() {
    let bad = [
        Snapshot::new(Dip(0.0), Dip(100.0)),
        Snapshot::new(Dip(100.0), Dip(0.4)),
        Snapshot::new(Dip(-5.0), Dip(100.0)),
        Snapshot::new(Dip(f32::NAN), Dip(100.0)),
        Snapshot::new(Dip(f32::INFINITY), Dip(100.0)),
        Snapshot::new(Dip(100_000.0), Dip(100.0)),
        Snapshot::new(Dip(100.0), Dip(100.0)).dpi(0),
        Snapshot::new(Dip(100.0), Dip(100.0)).dpi(100_000),
    ];
    for snapshot in bad {
        let result = try_render(snapshot, |ui| build(ui, Rc::default()));
        assert!(matches!(result, Err(SnapshotError::Size)));
    }
}

#[test]
fn a_failing_build_returns_its_error() {
    let result = try_render(snapshot(), |_ui: &mut Ui<Msg>| {
        Err::<Demo, _>(BackendError::CreateFailed("widget"))
    });
    assert!(matches!(
        result,
        Err(SnapshotError::Backend(BackendError::CreateFailed("widget")))
    ));
}

#[test]
fn a_click_in_the_step_toggles_a_checkbox_in_the_pixels() {
    let seen = Rc::new(Cell::new(0));
    let plain = shot(snapshot());
    let clicked = render_with(
        snapshot(),
        {
            let seen = Rc::clone(&seen);
            move |ui| build(ui, seen)
        },
        |stage| assert!(stage.click(30, 78), "the checkbox takes the click"),
    )
    .unwrap();
    assert_ne!(plain, clicked, "the checked box looks different");
    assert_eq!(seen.get(), 1, "the toggle message reached update");
}

#[test]
fn hovering_a_button_shows_in_the_pixels() {
    let plain = shot(snapshot());
    let hovered = render_with(
        snapshot(),
        |ui| build(ui, Rc::default()),
        |stage| {
            stage.hover(40, 36);
        },
    )
    .unwrap();
    assert_ne!(plain.pixel(40, 30), hovered.pixel(40, 30));
}

#[test]
fn a_message_in_the_step_reaches_the_app_before_the_capture() {
    let seen = Rc::new(Cell::new(0));
    let plain = shot(snapshot());
    let changed = render_with(
        snapshot(),
        {
            let seen = Rc::clone(&seen);
            move |ui| build(ui, seen)
        },
        |stage| stage.emit(Msg::Text("a much longer label text")),
    )
    .unwrap();
    assert_eq!(seen.get(), 1);
    assert_ne!(plain, changed, "the label repainted");
}

#[test]
fn render_matches_try_render() {
    let via_render = render(snapshot(), |ui| build(ui, Rc::default()).unwrap()).unwrap();
    assert_eq!(via_render, shot(snapshot()));
}

#[test]
fn an_open_dialog_dims_the_content_but_lets_it_show_through() {
    // The dialog's card is centred and clear of x=10 in a 320x200 window, so
    // both sampled pixels are scrim over content: (10, 48) over the button and
    // (10, 190) over the bare background.
    for theme in [Theme::light(), Theme::dark()] {
        let plain = render_with(
            snapshot().theme(theme),
            |ui| build_modal(ui, Rc::new(Cell::new(0)), false),
            |_| {},
        )
        .unwrap();
        let dimmed = render_with(
            snapshot().theme(theme),
            |ui| build_modal(ui, Rc::new(Cell::new(0)), true),
            |_| {},
        )
        .unwrap();

        let over_button = dimmed.pixel(10, 48).unwrap();
        let over_background = dimmed.pixel(10, 190).unwrap();
        assert!(
            brightness(over_button) < brightness(plain.pixel(10, 48).unwrap()),
            "{theme:?}: the scrim darkens the button it covers"
        );
        assert!(
            brightness(over_background) < brightness(plain.pixel(10, 190).unwrap()),
            "{theme:?}: the scrim darkens the background it covers"
        );
        assert_ne!(
            over_button, over_background,
            "{theme:?}: the scrim is translucent, so the content behind it still shows"
        );
    }
}

#[test]
fn an_open_dialog_blocks_clicks_to_the_content_behind_it() {
    let reached = Rc::new(Cell::new(0));
    render_with(
        snapshot(),
        {
            let reached = Rc::clone(&reached);
            move |ui| build_modal(ui, reached, false)
        },
        |stage| assert!(stage.click(10, 48), "the button takes the click"),
    )
    .unwrap();
    assert_eq!(reached.get(), 1, "without a dialog the button is clickable");

    let blocked = Rc::new(Cell::new(0));
    render_with(
        snapshot(),
        {
            let blocked = Rc::clone(&blocked);
            move |ui| build_modal(ui, blocked, true)
        },
        |stage| {
            // The scrim covers the whole client, so both the button and the
            // bare background are behind it and never receive the event.
            stage.click(10, 48);
            stage.click(10, 190);
        },
    )
    .unwrap();
    assert_eq!(blocked.get(), 0, "the scrim swallows clicks behind it");
}
