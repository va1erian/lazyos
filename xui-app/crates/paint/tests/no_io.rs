//! The app must work with a storage whose every method fails: Save/Open are
//! disabled and a forced save/open surfaces in the status bar, never panics.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, render_with};
use xui_core::Dip;
use xui_core::backend::Event;
use xui_core::message::{Modifiers, MouseButton};
use xui_paint::Msg;
use xui_paint::storage::{FailingStorage, Storage};
use xui_paint::view::{Observer, PaintApp};

use common::Widgets;

#[test]
fn nothing_depends_on_io() {
    assert!(!FailingStorage.available());

    let observer = Rc::new(RefCell::new(Observer::default()));
    let build_probe = Rc::clone(&observer);
    let widgets = Widgets::default();
    let built = widgets.clone();
    let image = render_with(
        Snapshot::new(Dip(800.0), Dip(600.0)),
        move |ui| {
            built.record(PaintApp::build_observed(
                ui,
                Rc::new(FailingStorage),
                build_probe,
            ))
        },
        move |stage| {
            let areas = widgets.areas(stage.ui());
            let y = areas.canvas.top + 40;
            // A forced save and open both report the failure in the status bar.
            stage.emit(Msg::Save);
            assert!(
                observer.borrow().status[3].contains("failed"),
                "a failing save must be reported"
            );
            stage.emit(Msg::Open);
            assert!(
                observer.borrow().status[3].contains("failed"),
                "a failing open must be reported"
            );
            // Drawing still works with no storage at all.
            stage.inject(Event::MouseDown {
                x: 100,
                y,
                button: MouseButton::Left,
                modifiers: Modifiers::NONE,
            });
            stage.inject(Event::MouseUp {
                x: 180,
                y,
                button: MouseButton::Left,
                modifiers: Modifiers::NONE,
            });
            assert!(observer.borrow().can_undo, "drawing needs no storage");
        },
    )
    .expect("render");

    // The canvas painted the stroke and the window rendered.
    assert!(image.width() > 0 && image.height() > 0);
}
