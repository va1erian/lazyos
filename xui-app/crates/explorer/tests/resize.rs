//! Window-resize tests: one open folder window re-flows its toolbar, its
//! icon view and its status bar when the window changes size. Driven through the resizable
//! offscreen backend, which delivers the window-level `Event::Resize`.

use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_app_testkit::TestBackend;
use xui_core::backend::{Backend, PlatformSpec, WidgetId, WindowId};
use xui_core::units::Dip;
use xui_core::{Ui, run_app};
use xui_explorer::platform::Launcher;
use xui_explorer::window::Msg;
use xui_explorer::{Explorer, ExplorerWindow, MemPlatform};

const WIDTH: i32 = 420;
const HEIGHT: i32 = 320;
/// The status bar's design height, in device pixels at 96 DPI.
const STATUS_HEIGHT: i32 = 24;
/// The toolbar's design height, in device pixels at 96 DPI.
const TOOLBAR_HEIGHT: i32 = 38;

/// A launcher that opens nothing; the resize test never activates an entry.
struct NoopLauncher;

impl Launcher for NoopLauncher {
    fn open(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }
}

/// The widget identities a resize check reads back.
struct Probe {
    ui: Ui<Msg>,
    view: WidgetId,
    status: WidgetId,
    window: WindowId,
}

/// Builds one folder window at 420x320, resizes it to `width` x `height` and
/// runs `check` against the re-flowed widgets.
fn resized(width: i32, height: i32, check: impl FnOnce(&Probe) + 'static) {
    let backend = Rc::new(TestBackend::new());
    let hook_backend = Rc::clone(&backend);
    let probe: Rc<std::cell::RefCell<Option<Probe>>> = Rc::new(std::cell::RefCell::new(None));
    let hook_probe = Rc::clone(&probe);
    backend.set_run_hook(move || {
        let probe = hook_probe.borrow_mut().take().expect("the app built");
        hook_backend.resize_window(probe.window, width, height);
        check(&probe);
    });

    let explorer = Explorer::new(
        Rc::new(
            MemPlatform::new()
                .dir("/a")
                .dir("/a/b")
                .file("/a/top.txt", 2),
        ),
        Rc::new(NoopLauncher),
    );
    let build_probe = Rc::clone(&probe);
    let handle: Rc<dyn Backend> = Rc::clone(&backend) as Rc<dyn Backend>;
    run_app(
        handle,
        PlatformSpec::new("files").size(Dip(WIDTH as f32), Dip(HEIGHT as f32)),
        move |ui| {
            let window = ExplorerWindow::new(ui, explorer, PathBuf::from("/a"))
                .expect("the explorer window built");
            *build_probe.borrow_mut() = Some(Probe {
                ui: ui.clone(),
                view: window.view_handle().id(),
                status: window.status_bar().id(),
                window: ui.window(),
            });
            window
        },
    )
    .expect("the resize session ran");
}

#[test]
fn a_larger_window_reflows_the_view_and_status_bar() {
    resized(700, 500, |probe| {
        let view = probe.ui.bounds(probe.view);
        let status = probe.ui.bounds(probe.status);
        assert_eq!(view.width(), 700, "the icon view spans the new width");
        assert_eq!(status.width(), 700, "the status bar spans the new width");
        assert_eq!(status.bottom, 500, "the status bar sits at the new bottom");
        assert_eq!(status.top, 500 - STATUS_HEIGHT);
        assert_eq!(
            view.bottom, status.top,
            "the view ends where the bar begins"
        );
        assert_eq!(
            view.top, TOOLBAR_HEIGHT,
            "the view starts under the toolbar"
        );
    });
}

#[test]
fn a_window_smaller_than_the_natural_size_does_not_panic() {
    resized(80, 40, |probe| {
        let view = probe.ui.bounds(probe.view);
        let status = probe.ui.bounds(probe.status);
        assert!(view.top <= view.bottom, "view: {view:?}");
        assert!(status.top <= status.bottom, "status: {status:?}");
    });
}

#[test]
fn the_minimum_window_reflows() {
    resized(200, 140, |probe| {
        let view = probe.ui.bounds(probe.view);
        let status = probe.ui.bounds(probe.status);
        assert_eq!(view.width(), 200, "the view follows the minimum");
        assert_eq!(status.width(), 200);
        assert_eq!(status.bottom, 140, "the bar is still at the bottom");
        assert!(view.top <= view.bottom && status.top <= status.bottom);
    });
}
