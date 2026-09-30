//! Runs the same scripted self-test `xpaint --self-test` runs, plus a headless
//! doodle render, so the model checks are covered under `cargo test` too.

use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, render_with};
use xui_core::Dip;
use xui_core::Theme;
use xui_core::backend::Event;
use xui_core::message::{Modifiers, MouseButton};
use xui_paint::storage::MemoryStorage;
use xui_paint::view::PaintApp;

#[test]
fn the_scripted_self_test_passes() {
    let mut out = Vec::new();
    assert!(
        xui_paint::selftest::run(&mut out),
        "{}",
        String::from_utf8_lossy(&out)
    );
    let text = String::from_utf8_lossy(&out);
    assert!(!text.contains("FAIL"), "{text}");
}

#[test]
fn a_headless_doodle_renders_and_the_themes_differ() {
    let doodle = |theme: Theme| {
        render_with(
            Snapshot::new(Dip(320.0), Dip(240.0)).theme(theme),
            |ui| PaintApp::build(ui, Rc::new(MemoryStorage::new())),
            |stage| {
                stage.emit(xui_paint::Msg::Tool(xui_paint::Tool::Pencil));
                let at = |x, y, button| Event::MouseDown {
                    x,
                    y,
                    button,
                    modifiers: Modifiers::NONE,
                };
                stage.inject(at(60, 100, MouseButton::Left));
                stage.inject(Event::MouseMove {
                    x: 120,
                    y: 140,
                    modifiers: Modifiers::NONE,
                });
                stage.inject(Event::MouseUp {
                    x: 120,
                    y: 140,
                    button: MouseButton::Left,
                    modifiers: Modifiers::NONE,
                });
            },
        )
        .expect("render")
    };
    let light = doodle(Theme::light());
    let dark = doodle(Theme::dark());
    let drew = (90..110).any(|x| {
        (110..130)
            .any(|y| matches!(light.pixel(x, y), Some([r, g, b, _]) if r < 80 && g < 80 && b < 80))
    });
    assert!(drew, "the doodle left no dark pixels");
    assert_ne!(
        light.pixel(1, 1),
        dark.pixel(1, 1),
        "light and dark chrome must differ"
    );
}
