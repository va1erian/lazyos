//! Window-resize tests: the widgets re-flow to the new client rect while the
//! document/bitmap and undo history keep their state. Driven through the
//! resizable offscreen backend, which delivers the window-level `Event::Resize`
//! the compositor sends on `Configure`.

use std::cell::RefCell;
use std::rc::Rc;

use xui_app_testkit::TestBackend;
use xui_core::backend::{Backend, Event, PlatformSpec, WidgetId, WindowId};
use xui_core::message::{Modifiers, MouseButton};
use xui_core::units::Dip;
use xui_core::{Ui, run_app};
use xui_paint::storage::MemoryStorage;
use xui_paint::view::{Observer, PaintApp};

const WIDTH: i32 = 800;
const HEIGHT: i32 = 600;

/// The widget identities and model state a resize check reads back.
struct Probe {
    ui: Ui<xui_paint::Msg>,
    backend: Rc<TestBackend>,
    observer: Rc<RefCell<Observer>>,
    canvas: WidgetId,
    toolbar: WidgetId,
    palette: WidgetId,
    status: WidgetId,
    window: WindowId,
    bitmap: (u32, u32),
}

impl Probe {
    /// Injects one pointer event at the window, as a backend would.
    fn pointer(&self, event: Event) {
        self.backend.inject(self.window, event);
    }
}

/// Builds Paint at 800x600, runs `setup`, resizes the window to `width` x
/// `height`, then runs `check` against the re-flowed widgets.
fn resized(
    width: i32,
    height: i32,
    setup: impl FnOnce(&Probe) + 'static,
    check: impl FnOnce(&Probe) + 'static,
) {
    let backend = Rc::new(TestBackend::new());
    let hook_backend = Rc::clone(&backend);
    let probe: Rc<RefCell<Option<Probe>>> = Rc::new(RefCell::new(None));
    let hook_probe = Rc::clone(&probe);
    backend.set_run_hook(move || {
        let probe = hook_probe.borrow_mut().take().expect("the app built");
        setup(&probe);
        hook_backend.resize_window(probe.window, width, height);
        check(&probe);
    });

    let build_probe = Rc::clone(&probe);
    let handle: Rc<dyn Backend> = Rc::clone(&backend) as Rc<dyn Backend>;
    run_app(
        handle,
        PlatformSpec::new("paint").size(Dip(WIDTH as f32), Dip(HEIGHT as f32)),
        move |ui| {
            let observer = Rc::new(RefCell::new(Observer::default()));
            let app =
                PaintApp::build_observed(ui, Rc::new(MemoryStorage::new()), Rc::clone(&observer))
                    .expect("the paint widgets built");
            *build_probe.borrow_mut() = Some(Probe {
                ui: ui.clone(),
                backend: Rc::clone(&backend),
                observer,
                canvas: app.canvas().id(),
                toolbar: app.toolbar().id(),
                palette: app.palette().id(),
                status: app.status().id(),
                window: ui.window(),
                bitmap: app.model().bitmap().size(),
            });
            app
        },
    )
    .expect("the resize session ran");
}

/// Asserts the widget stack re-flowed to `width` x `height`.
fn assert_reflowed(probe: &Probe, width: i32, height: i32) {
    let toolbar = probe.ui.bounds(probe.toolbar);
    let canvas = probe.ui.bounds(probe.canvas);
    let palette = probe.ui.bounds(probe.palette);
    let status = probe.ui.bounds(probe.status);
    assert_eq!(toolbar.width(), width, "the toolbar spans the new width");
    assert_eq!(canvas.width(), width, "the canvas spans the new width");
    assert_eq!(palette.width(), width, "the palette spans the new width");
    assert_eq!(status.width(), width, "the status bar spans the new width");
    assert_eq!(
        status.bottom, height,
        "the status bar sits at the new bottom"
    );
    assert!(
        toolbar.bottom <= canvas.top && canvas.bottom <= palette.top,
        "the stack stays ordered: {toolbar:?} {canvas:?} {palette:?}"
    );
    assert_eq!(probe.bitmap, (320, 240), "the document is unchanged");
}

#[test]
fn a_larger_window_reflows_every_widget() {
    resized(1000, 700, |_| {}, |probe| assert_reflowed(probe, 1000, 700));
}

#[test]
fn the_minimum_window_still_reflows_and_clamps() {
    // 320x240 is Paint's declared minimum; the widgets must follow it without
    // panicking on the small rectangles.
    resized(
        320,
        240,
        |_| {},
        |probe| {
            assert_reflowed(probe, 320, 240);
            let toolbar = probe.ui.bounds(probe.toolbar);
            let status = probe.ui.bounds(probe.status);
            assert!(toolbar.top <= toolbar.bottom && status.top <= status.bottom);
        },
    );
}

#[test]
fn a_window_smaller_than_the_natural_size_does_not_panic() {
    // Far below the declared minimum: the layout must clamp rather than make
    // an inverted or negative rectangle.
    resized(
        80,
        40,
        |_| {},
        |probe| {
            let toolbar = probe.ui.bounds(probe.toolbar);
            let canvas = probe.ui.bounds(probe.canvas);
            let palette = probe.ui.bounds(probe.palette);
            let status = probe.ui.bounds(probe.status);
            assert!(toolbar.top <= toolbar.bottom, "toolbar: {toolbar:?}");
            assert!(canvas.top <= canvas.bottom, "canvas: {canvas:?}");
            assert!(palette.top <= palette.bottom, "palette: {palette:?}");
            assert!(status.top <= status.bottom, "status: {status:?}");
            assert_eq!(probe.bitmap, (320, 240), "the document is unchanged");
        },
    );
}

#[test]
fn a_resize_keeps_the_drawing_and_the_undo_history() {
    resized(
        640,
        480,
        |probe| {
            // Draw one stroke on the canvas before the resize.
            let canvas = probe.ui.bounds(probe.canvas);
            let y = canvas.top + 40;
            let down = |x, y| Event::MouseDown {
                x,
                y,
                button: MouseButton::Left,
                modifiers: Modifiers::NONE,
            };
            probe.pointer(down(100, y));
            probe.pointer(Event::MouseMove {
                x: 200,
                y: y + 30,
                modifiers: Modifiers::NONE,
            });
            probe.pointer(Event::MouseUp {
                x: 200,
                y: y + 30,
                button: MouseButton::Left,
                modifiers: Modifiers::NONE,
            });
            assert!(probe.observer.borrow().can_undo, "the stroke is undoable");
        },
        |probe| {
            assert_reflowed(probe, 640, 480);
            assert!(
                probe.observer.borrow().can_undo,
                "the resize left the undo stack alone"
            );
        },
    );
}
