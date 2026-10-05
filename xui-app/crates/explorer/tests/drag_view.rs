#![forbid(unsafe_code)]

//! The view state a folder window publishes for the platform's drag and
//! drop: its folder, its icon view, the selection as paths, and the earlier
//! selection a press collapsed.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, render_with};
use xui_core::backend::BackendError;
use xui_core::units::Dip;
use xui_explorer::platform::Launcher;
use xui_explorer::window::Msg;
use xui_explorer::{Explorer, ExplorerWindow, MemPlatform};

struct NoLauncher;

impl Launcher for NoLauncher {
    fn open(&self, _path: &Path) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_window_publishes_what_a_drag_carries() {
    let platform = Rc::new(
        MemPlatform::new()
            .dir("/a")
            .file("/a/one.txt", 1)
            .file("/a/two.txt", 2)
            .file("/a/three.txt", 3),
    );
    let explorer = Explorer::new(platform, Rc::new(NoLauncher));
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    let (build, step, log) = (Rc::clone(&explorer), Rc::clone(&explorer), Rc::clone(&seen));
    let view: Rc<RefCell<Option<Rc<xui_core::widget::IconView<Msg>>>>> = Rc::default();
    let (view_build, view_step) = (Rc::clone(&view), Rc::clone(&view));
    let window_id: Rc<RefCell<u64>> = Rc::default();
    let (id_build, id_step) = (Rc::clone(&window_id), Rc::clone(&window_id));
    render_with(
        Snapshot::new(Dip(420.0), Dip(320.0)),
        move |ui| {
            let window = ExplorerWindow::new(ui, build, PathBuf::from("/a"))?;
            *view_build.borrow_mut() = Some(window.view_handle());
            *id_build.borrow_mut() = ui.window().raw();
            Ok::<_, BackendError>(window)
        },
        move |stage| {
            let view = view_step.borrow().clone().unwrap();
            let window = *id_step.borrow();
            let state = step.view_state(window).expect("published at build");
            log.borrow_mut().push(format!(
                "dir={} view={}",
                state.dir.display(),
                state.view == view.id()
            ));
            view.set_selection(&[0, 1, 2]);
            stage.emit(Msg::Selection);
            // A press on one tile collapses the selection to it.
            view.set_selection(&[1]);
            stage.emit(Msg::Selection);
            let state = step.view_state(window).unwrap();
            log.borrow_mut().push(format!(
                "carried={} collapsed={}",
                state.drag_paths().len(),
                state.collapsed()
            ));
            stage.emit(Msg::RestoreSelection);
            log.borrow_mut()
                .push(format!("restored={}", view.selection().len()));
        },
    )
    .expect("the headless render");
    assert_eq!(
        *seen.borrow(),
        ["dir=/a view=true", "carried=3 collapsed=true", "restored=3"]
    );
    // A closed window is forgotten.
    assert!(explorer.view_state(*window_id.borrow()).is_none());
}
