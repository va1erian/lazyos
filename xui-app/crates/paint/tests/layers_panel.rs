//! The Layers panel, Layer Properties, the History panel and the alpha slider,
//! driven through the offscreen `Stage` the way a session script drives them:
//! by the rectangles the UI probe reports.

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, Stage, render_with};
use xui_core::Dip;
use xui_core::Theme;
use xui_core::backend::Event;
use xui_core::geometry::Rect;
use xui_core::image::Image;
use xui_core::message::{Key, Modifiers, MouseButton};
use xui_paint::Msg;
use xui_paint::model::Tool;
use xui_paint::storage::MemoryStorage;
use xui_paint::view::{Command, Observer, PaintApp};

use common::driver::{ctrl, ctrl_shift, press};

type Stg<'a> = Stage<'a, Msg>;
type Obs = Rc<RefCell<Observer>>;
type Names = Rc<RefCell<BTreeMap<String, Rect>>>;

/// What a step reads: the observer and the probed rectangles.
struct Ctx {
    observer: Obs,
    names: Names,
}

impl Ctx {
    fn rect(&self, name: &str) -> Rect {
        *self
            .names
            .borrow()
            .get(name)
            .unwrap_or_else(|| panic!("{name} probed"))
    }

    fn has(&self, name: &str) -> bool {
        self.names.borrow().contains_key(name)
    }

    fn centre(&self, name: &str) -> (i32, i32) {
        let r = self.rect(name);
        ((r.left + r.right) / 2, (r.top + r.bottom) / 2)
    }

    fn layers(&self) -> Vec<(String, bool, u8)> {
        self.observer.borrow().layers.clone()
    }

    fn active(&self) -> usize {
        self.observer.borrow().active_layer
    }

    fn history(&self) -> (usize, usize) {
        self.observer.borrow().history
    }
}

/// Probes again (the names move with the layout) and clicks `name`'s centre.
fn click(stage: &Stg<'_>, ctx: &Ctx, name: &str) {
    stage.emit(Msg::Probe);
    click_at(stage, ctx.centre(name));
}

fn click_at(stage: &Stg<'_>, at: (i32, i32)) {
    for event in [
        Event::MouseDown {
            x: at.0,
            y: at.1,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        },
        Event::MouseUp {
            x: at.0,
            y: at.1,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        },
    ] {
        stage.inject(event);
    }
}

fn session_at(theme: Theme, dpi: u32, step: impl FnOnce(&Stg<'_>, &Ctx) + 'static) -> Image {
    let observer = Rc::new(RefCell::new(Observer::default()));
    let build_observer = Rc::clone(&observer);
    let names: Names = Rc::new(RefCell::new(BTreeMap::new()));
    let hook_names = Rc::clone(&names);
    render_with(
        Snapshot::new(Dip(800.0), Dip(600.0)).theme(theme).dpi(dpi),
        move |ui| {
            let mut app =
                PaintApp::build_observed(ui, Rc::new(MemoryStorage::new()), build_observer)?;
            app.set_probe(move |_, name, rect| {
                hook_names.borrow_mut().insert(name.to_string(), rect);
            });
            Ok(app)
        },
        move |stage| {
            stage.emit(Msg::Probe);
            step(stage, &Ctx { observer, names })
        },
    )
    .expect("render")
}

fn session(step: impl FnOnce(&Stg<'_>, &Ctx) + 'static) {
    session_at(Theme::light(), 96, step);
}

/// A short stroke on the active layer with the pencil, in canvas pixels.
fn stroke(stage: &Stg<'_>, ctx: &Ctx, at: (i32, i32)) {
    stage.emit(Msg::Probe);
    let canvas = ctx.rect("canvas");
    let doc = (320, 240);
    let mut viewport = xui_paint::viewport::Viewport::new();
    viewport.clamp(doc, (canvas.width(), canvas.height()));
    let (x, y) = viewport.to_view(at.0, at.1);
    let (x, y) = (canvas.left + x, canvas.top + y);
    stage.inject(Event::MouseDown {
        x,
        y,
        button: MouseButton::Left,
        modifiers: Modifiers::NONE,
    });
    stage.inject(Event::MouseUp {
        x: x + 10,
        y,
        button: MouseButton::Left,
        modifiers: Modifiers::NONE,
    });
}

#[test]
fn the_panel_lists_layers_top_first_and_its_buttons_work_with_undo() {
    session(|stage, ctx| {
        assert_eq!(ctx.layers().len(), 1);
        // Disabled while they cannot act: one layer cannot be deleted, merged
        // or moved.
        for name in ["layer_delete", "layer_merge", "layer_up", "layer_down"] {
            click(stage, ctx, name);
            assert_eq!(ctx.layers().len(), 1, "{name} is disabled");
            assert_eq!(ctx.history(), (0, 0), "{name} did nothing");
        }
        click(stage, ctx, "layer_add");
        assert_eq!(ctx.layers().len(), 2);
        assert_eq!(ctx.active(), 1, "the new layer is active");
        click(stage, ctx, "layer_duplicate");
        assert_eq!(ctx.layers().len(), 3);
        assert_eq!(ctx.layers()[2].0, "Layer 2 copy");
        // Rows are top first: layer 2 above layer 1 above the background.
        stage.emit(Msg::Probe);
        let (top, mid, bottom) = (
            ctx.rect("layer:2"),
            ctx.rect("layer:1"),
            ctx.rect("layer:0"),
        );
        assert!(top.top < mid.top && mid.top < bottom.top);

        click(stage, ctx, "layer_up");
        assert_eq!(ctx.active(), 2, "already on top: up is disabled");
        click(stage, ctx, "layer_down");
        assert_eq!(ctx.active(), 1, "moved down");
        click(stage, ctx, "layer_merge");
        assert_eq!(ctx.layers().len(), 2);
        click(stage, ctx, "layer_delete");
        assert_eq!(ctx.layers().len(), 1);

        // Every step undoes and redoes.
        for expected in [2, 3, 3, 2, 1] {
            ctrl(stage, Key::Z);
            assert_eq!(ctx.layers().len(), expected);
        }
        for expected in [2, 3] {
            ctrl(stage, Key::Y);
            assert_eq!(ctx.layers().len(), expected);
        }
    });
}

#[test]
fn clicking_a_row_selects_and_the_eye_hides_with_a_named_step() {
    session(|stage, ctx| {
        click(stage, ctx, "layer_add");
        click(stage, ctx, "layer_add");
        assert_eq!(ctx.active(), 2);
        click(stage, ctx, "layer:0");
        assert_eq!(ctx.active(), 0);
        assert_eq!(ctx.history().0, 2, "selecting is not an edit");
        click(stage, ctx, "layer_eye:1");
        assert!(!ctx.layers()[1].1, "the eye hid layer 1");
        assert_eq!(ctx.active(), 0, "the eye does not select");
        ctrl(stage, Key::Z);
        assert!(ctx.layers()[1].1);
        assert_eq!(
            ctx.observer.borrow().status[3],
            "Undid Hide Layer",
            "the step is named"
        );
    });
}

#[test]
fn every_layer_menu_command_and_key_runs_and_undoes() {
    session(|stage, ctx| {
        let n = || ctx.layers().len();
        ctrl_shift(stage, Key::N);
        assert_eq!(n(), 2);
        ctrl_shift(stage, Key::D);
        assert_eq!(n(), 3);
        stage.emit(Msg::Command(Command::MoveLayerDown));
        assert_eq!(ctx.active(), 1);
        stage.emit(Msg::Command(Command::MoveLayerUp));
        assert_eq!(ctx.active(), 2);
        ctrl(stage, Key::M);
        assert_eq!(n(), 2);
        stage.emit(Msg::Command(Command::FlipLayerHorizontal));
        stage.emit(Msg::Command(Command::FlipLayerVertical));
        stage.emit(Msg::Command(Command::DeleteLayer));
        assert_eq!(n(), 1);
        // Flatten needs layers; the orientation commands do not.
        stage.emit(Msg::Command(Command::Flatten));
        let steps = ctx.history().0;
        stage.emit(Msg::Command(Command::Rotate90Cw));
        stage.emit(Msg::Command(Command::Rotate90Ccw));
        stage.emit(Msg::Command(Command::Rotate180));
        stage.emit(Msg::Command(Command::FlipHorizontal));
        stage.emit(Msg::Command(Command::FlipVertical));
        assert_eq!(ctx.history().0, steps + 5);
        ctrl_shift(stage, Key::N);
        ctrl_shift(stage, Key::N);
        stage.emit(Msg::Command(Command::Flatten));
        assert_eq!(n(), 1, "Ctrl+Shift+F flattens");
        ctrl(stage, Key::Z);
        assert_eq!(n(), 3, "and undoes");
        ctrl_shift(stage, Key::F);
        assert_eq!(n(), 1);
        // Undo everything back to the one starting layer.
        let (applied, _) = ctx.history();
        for _ in 0..applied {
            ctrl(stage, Key::Z);
        }
        assert_eq!(ctx.history().0, 0);
        assert_eq!(n(), 1);
        ctx.observer.borrow_mut().status[3].clear();
    });
}

#[test]
fn layer_properties_ok_is_one_step_and_cancel_restores() {
    session(|stage, ctx| {
        click(stage, ctx, "layer_add");
        assert_eq!(ctx.history().0, 1);
        // F4 opens it on the active layer; a double click on a row does too.
        press(stage, Key::F4);
        assert!(ctx.observer.borrow().props_open);
        stage.emit(Msg::Probe);
        assert!(ctx.has("props_opacity"));
        // Half way along the slider.
        let r = ctx.rect("props_opacity");
        click_at(stage, ((r.left + r.right) / 2, (r.top + r.bottom) / 2));
        let opacity = ctx.layers()[1].2;
        assert!((100..=156).contains(&opacity), "live preview: {opacity}");
        assert_eq!(ctx.history().0, 1, "a preview is not a step");
        click(stage, ctx, "props_cancel");
        assert_eq!(ctx.layers()[1].2, 255, "Cancel restores");
        assert!(!ctx.observer.borrow().props_open);
        assert_eq!(ctx.history().0, 1);

        // Double click on the lower row opens it for that layer.
        stage.emit(Msg::Probe);
        let (x, y) = ctx.centre("layer:0");
        stage.inject(Event::MouseDoubleClick {
            x,
            y,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        });
        assert!(ctx.observer.borrow().props_open);
        assert_eq!(ctx.active(), 0);
        stage.emit(Msg::Probe);
        let r = ctx.rect("props_opacity");
        click_at(stage, (r.left + r.width() / 4, (r.top + r.bottom) / 2));
        let faded = ctx.layers()[0].2;
        assert!(faded < 120, "{faded}");
        click(stage, ctx, "props_ok");
        assert_eq!(ctx.history().0, 2, "OK is one step");
        ctrl(stage, Key::Z);
        assert_eq!(ctx.layers()[0].2, 255);
        ctrl(stage, Key::Y);
        assert_eq!(ctx.layers()[0].2, faded);
    });
}

#[test]
fn the_history_panel_jumps_and_collapses() {
    session(|stage, ctx| {
        let pencil = [Tool::Pencil];
        let _ = pencil;
        for x in [20, 60, 100, 140] {
            stroke(stage, ctx, (x, 40));
        }
        assert_eq!(ctx.history(), (4, 4));
        stage.emit(Msg::Probe);
        let list = ctx.rect("history");
        // Rows are "Start" then the four strokes; find the row pitch by
        // clicking down the list until a click lands on step 2.
        let mut landed = None;
        for y in (list.top + 2..list.bottom).step_by(2) {
            click_at(stage, (list.left + 20, y));
            if ctx.history().0 == 2 {
                landed = Some(y);
                break;
            }
        }
        assert!(landed.is_some(), "a row of the list jumps to step 2");
        assert_eq!(ctx.history(), (2, 4), "redo is still there");
        ctrl(stage, Key::Y);
        assert_eq!(ctx.history(), (3, 4));
        // Collapse: the list leaves the probe.
        click(stage, ctx, "history_toggle");
        stage.emit(Msg::Probe);
        let before = ctx.rect("layers");
        assert!(ctx.history().1 == 4);
        click(stage, ctx, "history_toggle");
        stage.emit(Msg::Probe);
        assert!(
            ctx.rect("layers").height() < before.height(),
            "the list took room back"
        );
    });
}

#[test]
fn the_history_jump_message_goes_both_ways() {
    session(|stage, ctx| {
        for x in [20, 60, 100] {
            stroke(stage, ctx, (x, 40));
        }
        stage.emit(Msg::HistoryJump(0));
        assert_eq!(ctx.history(), (0, 3));
        stage.emit(Msg::HistoryJump(3));
        assert_eq!(ctx.history(), (3, 3));
        stage.emit(Msg::HistoryJump(1));
        assert_eq!(ctx.history(), (1, 3));
        stage.emit(Msg::HistoryJump(99));
        assert_eq!(ctx.history(), (3, 3), "clamped at the end");
    });
}

#[test]
fn the_alpha_slider_edits_the_colour_and_cancel_restores_it() {
    session(|stage, ctx| {
        stage.emit(Msg::EditColor(xui_paint::model::Side::Primary));
        stage.emit(Msg::Probe);
        let r = ctx.rect("color_alpha");
        click_at(stage, (r.left + r.width() / 2, (r.top + r.bottom) / 2));
        let alpha = ctx.observer.borrow().primary[3];
        assert!((100..=156).contains(&alpha), "{alpha}");
        // The panel's own change keeps the alpha.
        stage.emit(Msg::ColorEditing([10, 20, 30, 255]));
        assert_eq!(ctx.observer.borrow().primary, [10, 20, 30, alpha]);
        stage.emit(Msg::ColorCancel);
        assert_eq!(ctx.observer.borrow().primary, [0, 0, 0, 255]);
        stage.emit(Msg::EditColor(xui_paint::model::Side::Primary));
        stage.emit(Msg::ColorAlpha(77));
        stage.emit(Msg::ColorDone);
        assert_eq!(ctx.observer.borrow().primary, [0, 0, 0, 77]);
    });
}

#[test]
fn new_transparent_starts_a_clear_image() {
    session(|stage, ctx| {
        stroke(stage, ctx, (20, 20));
        stage.emit(Msg::NewTransparent);
        assert_eq!(ctx.history(), (0, 0));
        assert_eq!(ctx.layers().len(), 1);
    });
}

#[test]
fn the_probe_names_the_layer_controls() {
    session(|stage, ctx| {
        stage.emit(Msg::Probe);
        for name in [
            "layers",
            "layer:0",
            "layer_eye:0",
            "layer_add",
            "layer_delete",
            "layer_duplicate",
            "layer_merge",
            "layer_up",
            "layer_down",
            "history_toggle",
            "history",
        ] {
            assert!(ctx.has(name), "{name}");
        }
        assert!(!ctx.has("props_ok"), "only while Layer Properties is open");
    });
}

/// Three layers: a background, a hidden one and a red one at half opacity,
/// with a few history steps.
fn busy(stage: &Stg<'_>, ctx: &Ctx) {
    stroke(stage, ctx, (30, 30));
    click(stage, ctx, "layer_add");
    stage.emit(Msg::Command(Command::FlipLayerHorizontal));
    stage.emit(Msg::HistoryJump(2));
    stage.emit(Msg::HistoryJump(3));
    click(stage, ctx, "layer_add");
    stage.emit(Msg::EditColor(xui_paint::model::Side::Primary));
    stage.emit(Msg::ColorEditing([220, 30, 30, 255]));
    stage.emit(Msg::ColorDone);
    stage.emit(Msg::Tool(Tool::Rectangle));
    let canvas = ctx.rect("canvas");
    let (x, y) = (canvas.left + 150, canvas.top + 100);
    stage.inject(Event::MouseDown {
        x,
        y,
        button: MouseButton::Left,
        modifiers: Modifiers::NONE,
    });
    stage.inject(Event::MouseMove {
        x: x + 120,
        y: y + 80,
        modifiers: Modifiers::NONE,
    });
    stage.inject(Event::MouseUp {
        x: x + 120,
        y: y + 80,
        button: MouseButton::Left,
        modifiers: Modifiers::NONE,
    });
    stage.emit(Msg::LayerEye(1));
    stage.emit(Msg::LayerProps(2));
    stage.emit(Msg::Probe);
    let r = ctx.rect("props_opacity");
    click_at(stage, (r.left + r.width() / 2, (r.top + r.bottom) / 2));
    click(stage, ctx, "props_ok");
    assert!((100..=156).contains(&ctx.layers()[2].2), "half opacity");
}

#[test]
fn snapshots_of_three_layers_and_the_history_at_two_dpis_in_both_themes() {
    std::fs::create_dir_all("target/snapshots").unwrap();
    for dpi in [96, 192] {
        for (name, theme) in [("light", Theme::light()), ("dark", Theme::dark())] {
            let image = session_at(theme, dpi, |stage, ctx| {
                busy(stage, ctx);
                assert_eq!(ctx.layers().len(), 3);
                assert!(!ctx.layers()[1].1, "one layer is hidden");
            });
            image
                .save_png(format!("target/snapshots/layers-{dpi}dpi-{name}.png"))
                .expect("save snapshot");
        }
    }
}
