#![forbid(unsafe_code)]

//! Copy, paste, reveal and the selection feed, headless over an in-memory
//! filesystem and a recording [`Session`].

use std::cell::RefCell;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, render_with};
use xui_core::backend::BackendError;
use xui_core::units::Dip;
use xui_core::widget::{IconView, StatusBar};
use xui_explorer::platform::{Launcher, Pasted, Platform, Session};
use xui_explorer::window::Msg;
use xui_explorer::{Explorer, MemPlatform};

struct NoLauncher;

impl Launcher for NoLauncher {
    fn open(&self, _path: &Path) -> io::Result<()> {
        Err(io::Error::new(io::ErrorKind::Unsupported, "none"))
    }
}

/// Records copies and selections; a paste "copies" the last copied paths by
/// creating files of the same name in the folder of the in-memory platform.
struct Recorder {
    platform: Rc<MemPlatform>,
    copied: RefCell<Vec<Vec<PathBuf>>>,
    pasted_into: RefCell<Vec<PathBuf>>,
    selections: RefCell<Vec<(PathBuf, Vec<PathBuf>)>>,
}

impl Session for Recorder {
    fn copy(&self, paths: &[PathBuf]) -> io::Result<()> {
        self.copied.borrow_mut().push(paths.to_vec());
        Ok(())
    }

    fn paste_into(&self, dir: &Path) -> io::Result<Pasted> {
        self.pasted_into.borrow_mut().push(dir.to_path_buf());
        let last = self.copied.borrow().last().cloned().unwrap_or_default();
        for path in &last {
            let name = path.file_name().expect("a name");
            self.platform
                .add_file(&dir.join(format!("{} (2)", name.to_string_lossy())), 1);
        }
        Ok(Pasted {
            copied: last.len(),
            failed: Vec::new(),
        })
    }

    fn selection_changed(&self, dir: &Path, paths: &[PathBuf]) {
        self.selections
            .borrow_mut()
            .push((dir.to_path_buf(), paths.to_vec()));
    }
}

struct Handles {
    view: Rc<IconView<Msg>>,
    status: Rc<StatusBar<Msg>>,
}

/// One window on `/a` (revealing `reveal` when given), then `step`.
fn drive(
    reveal: Option<&'static str>,
    step: impl FnOnce(&xui_canvas::snapshot::Stage<'_, Msg>, &Handles) + 'static,
) -> (Rc<Recorder>, Handles) {
    let platform = Rc::new(
        MemPlatform::new()
            .dir("/a")
            .dir("/a/b")
            .file("/a/note.txt", 2)
            .file("/a/top.txt", 2),
    );
    let session = Rc::new(Recorder {
        platform: Rc::clone(&platform),
        copied: RefCell::new(Vec::new()),
        pasted_into: RefCell::new(Vec::new()),
        selections: RefCell::new(Vec::new()),
    });
    let explorer = Explorer::with_session(
        platform as Rc<dyn Platform>,
        Rc::new(NoLauncher),
        Rc::clone(&session) as Rc<dyn Session>,
    );
    let slot: Rc<RefCell<Option<Handles>>> = Rc::new(RefCell::new(None));
    let (build, run) = (Rc::clone(&slot), Rc::clone(&slot));
    render_with(
        Snapshot::new(Dip(420.0), Dip(320.0)),
        move |ui| {
            let window = match reveal {
                Some(name) => {
                    explorer
                        .reveal_root(ui, PathBuf::from("/a"), OsStr::new(name))
                        .0
                }
                None => explorer.open_root(ui, PathBuf::from("/a")),
            };
            *build.borrow_mut() = Some(Handles {
                view: window.view_handle(),
                status: window.status_bar(),
            });
            Ok::<_, BackendError>(window)
        },
        move |stage| {
            if let Some(handles) = run.borrow().as_ref() {
                step(stage, handles);
            }
        },
    )
    .expect("the headless render");
    let handles = slot.borrow_mut().take().expect("handles");
    (session, handles)
}

#[test]
fn copy_puts_the_selection_on_the_clipboard() {
    let (session, handles) = drive(None, |stage, handles| {
        // Folders first: b, note.txt, top.txt.
        handles.view.set_selection(&[1, 2]);
        stage.emit(Msg::Copy);
    });
    assert_eq!(
        *session.copied.borrow(),
        vec![vec![
            PathBuf::from("/a/note.txt"),
            PathBuf::from("/a/top.txt")
        ]]
    );
    assert_eq!(handles.status.text(0).as_deref(), Some("Copied 2 items"));
}

#[test]
fn copy_with_nothing_selected_does_nothing() {
    let (session, _) = drive(None, |stage, handles| {
        handles.view.set_selection(&[]);
        stage.emit(Msg::Copy);
    });
    assert!(session.copied.borrow().is_empty());
}

#[test]
fn paste_copies_into_the_folder_and_refreshes() {
    let (session, handles) = drive(None, |stage, handles| {
        handles.view.set_selection(&[1]);
        stage.emit(Msg::Copy);
        stage.emit(Msg::Paste);
    });
    assert_eq!(*session.pasted_into.borrow(), vec![PathBuf::from("/a")]);
    assert_eq!(handles.view.len(), 4, "the pasted copy is listed");
    assert_eq!(handles.status.text(0).as_deref(), Some("Pasted 1 item"));
}

#[test]
fn reveal_selects_the_item_and_announces_it() {
    let (session, handles) = drive(Some("top.txt"), |_, _| {});
    assert_eq!(handles.view.selection(), vec![2]);
    let selections = session.selections.borrow();
    let last = selections.last().expect("a selection was announced");
    assert_eq!(last.0, PathBuf::from("/a"));
    assert_eq!(last.1, vec![PathBuf::from("/a/top.txt")]);
}

#[test]
fn revealing_a_missing_name_leaves_the_default_selection() {
    let (_, plain) = drive(None, |_, _| {});
    let (_, handles) = drive(Some("gone.txt"), |_, _| {});
    assert_eq!(handles.view.selection(), plain.view.selection());
}

#[test]
fn a_selection_change_is_announced_as_paths() {
    let (session, _) = drive(None, |stage, handles| {
        handles.view.set_selection(&[0]);
        stage.emit(Msg::Selection);
    });
    let selections = session.selections.borrow();
    assert_eq!(
        selections.last().map(|(_, paths)| paths.clone()),
        Some(vec![PathBuf::from("/a/b")])
    );
}
