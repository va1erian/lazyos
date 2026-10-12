//! The UI probe hook: the named rectangles a session script clicks, checked by
//! clicking the centre of each one the way `click_at` does.

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, Stage, render_with};
use xui_core::Dip;
use xui_core::backend::Event;
use xui_core::geometry::Rect;
use xui_core::message::{Modifiers, MouseButton};
use xui_paint::Msg;
use xui_paint::model::{Side, Tool};
use xui_paint::storage::MemoryStorage;
use xui_paint::view::{Observer, PaintApp};

type Names = Rc<RefCell<BTreeMap<String, (String, Rect)>>>;

/// Runs `step` with the app's observer and the names the probe reported.
fn session(step: impl FnOnce(&Stage<'_, Msg>, &Rc<RefCell<Observer>>, &Names) + 'static) {
    let observer = Rc::new(RefCell::new(Observer::default()));
    let build_probe = Rc::clone(&observer);
    let names: Names = Rc::new(RefCell::new(BTreeMap::new()));
    let hook_names = Rc::clone(&names);
    render_with(
        Snapshot::new(Dip(800.0), Dip(600.0)),
        move |ui| {
            let mut app = PaintApp::build_observed(ui, Rc::new(MemoryStorage::new()), build_probe)?;
            app.set_probe(move |window, name, rect| {
                hook_names
                    .borrow_mut()
                    .insert(name.to_string(), (window.to_string(), rect));
            });
            Ok(app)
        },
        move |stage| step(stage, &observer, &names),
    )
    .expect("render");
}

fn click_name(stage: &Stage<'_, Msg>, names: &Names, name: &str, button: MouseButton) {
    let rect = names
        .borrow()
        .get(name)
        .unwrap_or_else(|| panic!("{name} probed"))
        .1;
    let (x, y) = ((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2);
    for event in [
        Event::MouseDown {
            x,
            y,
            button,
            modifiers: Modifiers::NONE,
        },
        Event::MouseUp {
            x,
            y,
            button,
            modifiers: Modifiers::NONE,
        },
    ] {
        stage.inject(event);
    }
}

#[test]
fn the_probe_names_every_control_a_session_clicks() {
    session(|stage, _, names| {
        stage.emit(Msg::Probe);
        let seen = names.borrow();
        for name in [
            "canvas",
            "menubar",
            "size",
            "swatch:primary",
            "swatch:secondary",
            "swap_colors",
            "reset_colors",
            "palette:0",
            "palette:31",
        ] {
            assert!(seen.contains_key(name), "{name} is probed");
        }
        for tool in Tool::ALL {
            assert!(
                seen.contains_key(&format!("tool:{}", tool.label())),
                "{tool:?}"
            );
        }
        assert!(!seen.contains_key("color_ok"), "the editor is closed");
        let windows: Vec<&String> = seen.values().map(|(window, _)| window).collect();
        assert!(
            windows.iter().all(|window| window.ends_with("- Paint")),
            "{windows:?}"
        );
    });
}

#[test]
fn clicking_probed_rectangles_drives_the_app() {
    session(|stage, observer, names| {
        stage.emit(Msg::Probe);
        click_name(stage, names, "tool:Rectangle", MouseButton::Left);
        assert_eq!(observer.borrow().tool, Tool::Rectangle);
        click_name(stage, names, "palette:3", MouseButton::Left);
        assert_eq!(observer.borrow().primary, [255, 0, 0, 255]);
        click_name(stage, names, "palette:10", MouseButton::Right);
        assert_eq!(observer.borrow().secondary, [0, 0, 255, 255]);
        click_name(stage, names, "swap_colors", MouseButton::Left);
        assert_eq!(observer.borrow().primary, [0, 0, 255, 255]);
        click_name(stage, names, "reset_colors", MouseButton::Left);
        assert_eq!(observer.borrow().primary, [0, 0, 0, 255]);

        // The swatches open the editor, whose buttons the probe then names.
        click_name(stage, names, "swatch:primary", MouseButton::Left);
        assert_eq!(observer.borrow().editing, Some(Side::Primary));
        stage.emit(Msg::Probe);
        stage.emit(Msg::ColorEditing([1, 2, 3, 255]));
        click_name(stage, names, "color_cancel", MouseButton::Left);
        assert_eq!(observer.borrow().editing, None, "Cancel closed the editor");
        assert_eq!(
            observer.borrow().primary,
            [0, 0, 0, 255],
            "and restored the colour"
        );

        click_name(stage, names, "swatch:secondary", MouseButton::Left);
        assert_eq!(observer.borrow().editing, Some(Side::Secondary));
        stage.emit(Msg::Probe);
        stage.emit(Msg::ColorEditing([9, 8, 7, 255]));
        click_name(stage, names, "color_ok", MouseButton::Left);
        assert_eq!(observer.borrow().editing, None, "OK closed the editor");
        assert_eq!(
            observer.borrow().secondary,
            [9, 8, 7, 255],
            "and kept the colour"
        );
    });
}
