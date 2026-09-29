#![forbid(unsafe_code)]

//! Runs one snapshot session: build, step, capture, close.

use std::cell::RefCell;
use std::rc::Rc;

use xui_core::app::{App, Ui, run_app};
use xui_core::backend::{Backend, BackendError, PlatformSpec};
use xui_core::image::Image;

use super::{Snapshot, SnapshotError, Stage};
use crate::OffscreenBackend;

/// The app `run_app` hosts: the caller's, or a stand-in after a failed build
/// (`run_app` cannot fail from inside its build closure).
enum Hosted<A> {
    App(A),
    Failed,
}

impl<A: App> App for Hosted<A> {
    type Msg = A::Msg;

    fn update(&mut self, msg: A::Msg, ui: &mut Ui<A::Msg>) {
        if let Hosted::App(app) = self {
            app.update(msg, ui);
        }
    }
}

/// Runs the session on `backend` and returns the captured frame.
///
/// The capture is scheduled as the backend's run hook, which `run_app` reaches
/// after the app is built, installed and has drained its first messages, and
/// before the window is closed.
pub(super) fn capture_on<A, F, S>(
    backend: &Rc<OffscreenBackend>,
    snapshot: &Snapshot,
    build: F,
    step: S,
) -> Result<Image, SnapshotError>
where
    A: App,
    F: FnOnce(&mut Ui<A::Msg>) -> Result<A, BackendError>,
    S: FnOnce(&Stage<'_, A::Msg>) + 'static,
{
    let spec = PlatformSpec::new(&snapshot.title).size(snapshot.width, snapshot.height);
    let ui_slot: Rc<RefCell<Option<Ui<A::Msg>>>> = Rc::new(RefCell::new(None));
    let frame: Rc<RefCell<Option<Result<Image, SnapshotError>>>> = Rc::new(RefCell::new(None));
    let failure: Rc<RefCell<Option<BackendError>>> = Rc::new(RefCell::new(None));

    // The hook lives in the backend, so it holds the backend weakly.
    let weak = Rc::downgrade(backend);
    let hook_ui = Rc::clone(&ui_slot);
    let hook_frame = Rc::clone(&frame);
    backend.set_run_hook(move || {
        let (Some(backend), Some(ui)) = (weak.upgrade(), hook_ui.borrow_mut().take()) else {
            return;
        };
        let stage = Stage::new(&backend, ui);
        step(&stage);
        backend.pump(stage.window());
        let captured = backend
            .render(stage.window())
            .ok_or(SnapshotError::NoFrame)
            .and_then(|image| {
                Image::from_rgba(image.width, image.height, image.pixels)
                    .map_err(SnapshotError::Image)
            });
        *hook_frame.borrow_mut() = Some(captured);
    });

    let theme = snapshot.theme;
    let make_ui = Rc::clone(&ui_slot);
    let make_failure = Rc::clone(&failure);
    let handle: Rc<dyn Backend> = backend.clone();
    let ran = run_app(handle, spec, move |ui| {
        ui.set_theme(theme);
        match build(ui) {
            Ok(app) => {
                *make_ui.borrow_mut() = Some(ui.clone());
                Hosted::App(app)
            }
            Err(error) => {
                *make_failure.borrow_mut() = Some(error);
                Hosted::Failed
            }
        }
    });
    // Drop what a run that never reached the hook would leave behind.
    ui_slot.borrow_mut().take();
    ran?;
    if let Some(error) = failure.borrow_mut().take() {
        return Err(error.into());
    }
    let captured = frame.borrow_mut().take();
    captured.unwrap_or(Err(SnapshotError::NoFrame))
}
