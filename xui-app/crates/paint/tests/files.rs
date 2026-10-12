//! The Open / Save As / Resize dialogs driven through the offscreen stage over
//! a fake filesystem and an in-memory path store.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, Stage, render_with};
use xui_core::Dip;
use xui_core::backend::{Event, WidgetId};
use xui_core::geometry::Rect;
use xui_core::message::{Key, Modifiers, MouseButton};
use xui_core::widget::{Entry, FileSystem};
use xui_paint::Msg;
use xui_paint::model::Bitmap;
use xui_paint::storage::Storage;
use xui_paint::view::{Observer, PaintApp};

/// Files by absolute path, shared by the fake filesystem and the store.
#[derive(Default)]
struct Disk {
    files: RefCell<BTreeMap<PathBuf, Vec<u8>>>,
    fail_writes: Cell<bool>,
}

struct FakeFs(Rc<Disk>);

impl FileSystem for FakeFs {
    fn list(&self, dir: &Path) -> io::Result<Vec<Entry>> {
        Ok(self
            .0
            .files
            .borrow()
            .keys()
            .filter(|path| path.parent() == Some(dir))
            .map(|path| Entry {
                name: OsString::from(path.file_name().unwrap()),
                is_dir: false,
                size: None,
                modified: None,
            })
            .collect())
    }

    fn is_dir(&self, path: &Path) -> bool {
        path == Path::new("/")
    }

    fn exists(&self, path: &Path) -> bool {
        self.is_dir(path) || self.0.files.borrow().contains_key(path)
    }

    fn home(&self) -> Option<PathBuf> {
        Some(PathBuf::from("/"))
    }

    fn roots(&self) -> Vec<PathBuf> {
        vec![PathBuf::from("/")]
    }
}

struct DiskStorage {
    disk: Rc<Disk>,
    startup: Option<PathBuf>,
}

impl Storage for DiskStorage {
    fn save(&self, _bytes: &[u8]) -> Result<(), String> {
        Err("path-less save is not used with dialogs".to_string())
    }

    fn load(&self) -> Option<Vec<u8>> {
        self.load_from(self.startup.as_deref()?)
    }

    fn available(&self) -> bool {
        true
    }

    fn save_to(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        if self.disk.fail_writes.get() {
            return Err("disk full".to_string());
        }
        self.disk
            .files
            .borrow_mut()
            .insert(path.to_path_buf(), bytes.to_vec());
        Ok(())
    }

    fn load_from(&self, path: &Path) -> Option<Vec<u8>> {
        self.disk.files.borrow().get(path).cloned()
    }

    fn supports_paths(&self) -> bool {
        true
    }

    fn default_path(&self) -> Option<PathBuf> {
        self.startup.clone()
    }
}

type Seen = Rc<RefCell<Observer>>;

/// Builds the app over `disk` and runs `step` with the canvas's placed rect.
fn session(
    disk: &Rc<Disk>,
    startup: Option<&str>,
    step: impl FnOnce(&Stage<'_, Msg>, &Seen, Rect) + 'static,
) {
    let canvas: Rc<Cell<Option<WidgetId>>> = Rc::new(Cell::new(None));
    let built = Rc::clone(&canvas);
    let observer: Seen = Rc::new(RefCell::new(Observer::default()));
    let probe = Rc::clone(&observer);
    let storage = Rc::new(DiskStorage {
        disk: Rc::clone(disk),
        startup: startup.map(PathBuf::from),
    });
    let fs: Rc<dyn FileSystem> = Rc::new(FakeFs(Rc::clone(disk)));
    render_with(
        Snapshot::new(Dip(800.0), Dip(600.0)),
        move |ui| {
            let app = PaintApp::build_with_files_observed(ui, storage, fs, probe)?;
            app.set_start_dir("/");
            built.set(Some(app.canvas().id()));
            Ok(app)
        },
        move |stage| {
            let rect = stage.ui().bounds(canvas.get().expect("built"));
            step(stage, &observer, rect)
        },
    )
    .expect("render");
}

fn key(stage: &Stage<'_, Msg>, key: Key) {
    stage.inject(Event::KeyDown {
        key,
        modifiers: Modifiers::NONE,
        repeat: 1,
        system: false,
    });
}

fn type_text(stage: &Stage<'_, Msg>, text: &str) {
    stage.inject(Event::SetFocus);
    for c in text.chars() {
        stage.inject(Event::Char(c));
    }
}

/// Replaces the focused field's text: select all, then type over it.
fn replace_text(stage: &Stage<'_, Msg>, text: &str) {
    // The offscreen backend records focus but never delivers `SetFocus`, which
    // an `Edit` needs before it accepts typing.
    stage.inject(Event::SetFocus);
    stage.inject(Event::KeyDown {
        key: Key::A,
        modifiers: Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        },
        repeat: 1,
        system: false,
    });
    type_text(stage, text);
}

fn status(seen: &Seen) -> String {
    seen.borrow().status[3].clone()
}

fn size(seen: &Seen) -> String {
    seen.borrow().status[1].clone()
}

fn png(width: u32, height: u32) -> Vec<u8> {
    Bitmap::white(width, height).encode_png().unwrap()
}

/// Commands go through their shortcuts (Ctrl+S, Ctrl+O, Ctrl+Shift+R), which
/// exercises the window's key hook.
fn chord(stage: &Stage<'_, Msg>, key: Key, shift: bool) {
    stage.inject(Event::KeyDown {
        key,
        modifiers: Modifiers {
            ctrl: true,
            shift,
            ..Modifiers::NONE
        },
        repeat: 1,
        system: false,
    });
}

fn save(stage: &Stage<'_, Msg>) {
    chord(stage, Key::S, false)
}
fn open(stage: &Stage<'_, Msg>) {
    chord(stage, Key::O, false)
}
fn ask_size(stage: &Stage<'_, Msg>) {
    chord(stage, Key::R, true)
}

/// The window position of picture pixel `(x, y)` (centred picture, real viewport).
fn at(canvas: Rect, x: i32, y: i32) -> (i32, i32) {
    let mut viewport = xui_paint::viewport::Viewport::new();
    viewport.clamp((320, 240), (canvas.width(), canvas.height()));
    let (vx, vy) = viewport.to_view(x, y);
    (canvas.left + vx, canvas.top + vy)
}

/// One click 60 px right of and 40 px below the canvas's corner.
fn draw_a_dot(stage: &Stage<'_, Msg>, canvas: Rect) {
    let (x, y) = at(canvas, 60, 40);
    for event in [
        Event::MouseDown {
            x,
            y,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        },
        Event::MouseUp {
            x,
            y,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        },
    ] {
        stage.inject(event);
    }
}

#[test]
fn open_loads_the_chosen_png() {
    let disk = Rc::new(Disk::default());
    disk.files.borrow_mut().insert("/a.png".into(), png(50, 30));
    session(&disk, None, |stage, seen, _canvas| {
        open(stage);
        type_text(stage, "a.png");
        key(stage, Key::RETURN);
        assert_eq!(status(seen), "Opened a.png");
        assert_eq!(size(seen), "50 x 30");
        assert!(!seen.borrow().can_undo, "opening starts a fresh history");
    });
}

#[test]
fn a_corrupt_png_keeps_the_canvas() {
    let disk = Rc::new(Disk::default());
    disk.files
        .borrow_mut()
        .insert("/bad.png".into(), b"not a png".to_vec());
    session(&disk, None, |stage, seen, canvas| {
        draw_a_dot(stage, canvas);
        assert!(seen.borrow().can_undo);
        open(stage);
        type_text(stage, "bad.png");
        key(stage, Key::RETURN);
        assert!(status(seen).starts_with("Open failed"), "{}", status(seen));
        assert_eq!(size(seen), "320 x 240");
        assert!(seen.borrow().can_undo, "the document was not replaced");
    });
}

#[test]
fn cancelling_the_open_dialog_does_nothing() {
    let disk = Rc::new(Disk::default());
    disk.files.borrow_mut().insert("/a.png".into(), png(50, 30));
    session(&disk, None, |stage, seen, canvas| {
        draw_a_dot(stage, canvas);
        open(stage);
        key(stage, Key::ESCAPE);
        assert_eq!(status(seen), "");
        assert_eq!(size(seen), "320 x 240");
        assert!(seen.borrow().can_undo);
        // Drawing works again once the dialog is gone.
        let (x, y) = at(canvas, 90, 90);
        stage.inject(Event::MouseMove {
            x,
            y,
            modifiers: Modifiers::NONE,
        });
        assert_eq!(seen.borrow().cursor, Some((90, 90)));
    });
}

#[test]
fn save_writes_a_png_and_a_second_save_reuses_the_path() {
    let disk = Rc::new(Disk::default());
    let probe = Rc::clone(&disk);
    session(&disk, None, move |stage, seen, canvas| {
        draw_a_dot(stage, canvas);
        save(stage);
        // The suggested name is untitled.png.
        key(stage, Key::RETURN);
        assert_eq!(status(seen), "Saved untitled.png");
        let bytes = probe
            .files
            .borrow()
            .get(Path::new("/untitled.png"))
            .cloned();
        let saved = Bitmap::decode(&bytes.expect("the file was written")).unwrap();
        assert_eq!(saved.size(), (320, 240));

        // A second Save writes there without a dialog.
        probe.files.borrow_mut().clear();
        save(stage);
        assert_eq!(status(seen), "Saved untitled.png");
        assert!(
            probe
                .files
                .borrow()
                .contains_key(Path::new("/untitled.png"))
        );
    });
}

#[test]
fn save_appends_png_when_the_extension_is_missing() {
    let disk = Rc::new(Disk::default());
    let probe = Rc::clone(&disk);
    session(&disk, None, move |stage, seen, _canvas| {
        save(stage);
        replace_text(stage, "drawing");
        key(stage, Key::RETURN);
        assert_eq!(status(seen), "Saved drawing.png");
        assert!(probe.files.borrow().contains_key(Path::new("/drawing.png")));
    });
}

#[test]
fn an_appended_extension_never_replaces_an_unseen_file() {
    let disk = Rc::new(Disk::default());
    disk.files
        .borrow_mut()
        .insert("/drawing.png".into(), b"precious".to_vec());
    let probe = Rc::clone(&disk);
    session(&disk, None, move |stage, seen, _canvas| {
        save(stage);
        replace_text(stage, "drawing");
        key(stage, Key::RETURN);
        assert!(status(seen).starts_with("Not saved"), "{}", status(seen));
        assert_eq!(
            probe.files.borrow().get(Path::new("/drawing.png")).unwrap(),
            b"precious"
        );
    });
}

#[test]
fn overwriting_an_existing_file_asks_the_dialog_to_confirm() {
    let disk = Rc::new(Disk::default());
    disk.files
        .borrow_mut()
        .insert("/untitled.png".into(), b"old".to_vec());
    let probe = Rc::clone(&disk);
    session(&disk, None, move |stage, seen, _canvas| {
        save(stage);
        key(stage, Key::RETURN);
        assert_eq!(status(seen), "", "the picker is confirming, nothing saved");
        assert_eq!(
            probe
                .files
                .borrow()
                .get(Path::new("/untitled.png"))
                .unwrap(),
            b"old"
        );
        key(stage, Key::RETURN);
        assert_eq!(status(seen), "Saved untitled.png");
        assert_ne!(
            probe
                .files
                .borrow()
                .get(Path::new("/untitled.png"))
                .unwrap(),
            b"old"
        );
    });
}

#[test]
fn a_failed_write_reports_and_keeps_the_document() {
    let disk = Rc::new(Disk::default());
    disk.fail_writes.set(true);
    let probe = Rc::clone(&disk);
    session(&disk, None, move |stage, seen, canvas| {
        draw_a_dot(stage, canvas);
        save(stage);
        key(stage, Key::RETURN);
        assert!(status(seen).starts_with("Save failed"), "{}", status(seen));
        assert!(status(seen).contains("disk full"));
        assert!(seen.borrow().can_undo, "the document is intact");
        assert!(probe.files.borrow().is_empty());

        // The path was not adopted: the next Save asks again, and works once
        // the disk recovers.
        probe.fail_writes.set(false);
        save(stage);
        key(stage, Key::RETURN);
        assert_eq!(status(seen), "Saved untitled.png");
    });
}

#[test]
fn cancelling_save_as_writes_nothing() {
    let disk = Rc::new(Disk::default());
    let probe = Rc::clone(&disk);
    session(&disk, None, move |stage, seen, _canvas| {
        save(stage);
        key(stage, Key::ESCAPE);
        assert_eq!(status(seen), "");
        assert!(probe.files.borrow().is_empty());
    });
}

#[test]
fn the_start_up_file_loads_without_a_dialog_and_becomes_the_save_target() {
    let disk = Rc::new(Disk::default());
    disk.files
        .borrow_mut()
        .insert("/start.png".into(), png(40, 20));
    let probe = Rc::clone(&disk);
    session(&disk, Some("/start.png"), move |stage, seen, _canvas| {
        stage.emit(Msg::OpenStartup);
        assert_eq!(size(seen), "40 x 20");
        probe.files.borrow_mut().clear();
        stage.emit(Msg::Save);
        assert_eq!(status(seen), "Saved start.png");
        assert!(probe.files.borrow().contains_key(Path::new("/start.png")));
    });
}

#[test]
fn a_missing_start_up_file_is_reported() {
    let disk = Rc::new(Disk::default());
    session(&disk, Some("/gone.png"), |stage, seen, _canvas| {
        stage.emit(Msg::OpenStartup);
        assert!(status(seen).starts_with("Open failed"));
        assert_eq!(size(seen), "320 x 240");
    });
}

#[test]
fn resize_prompt_is_prefilled_applies_and_is_undoable() {
    let disk = Rc::new(Disk::default());
    session(&disk, None, |stage, seen, _canvas| {
        ask_size(stage);
        // Prefilled "320x240": erase it and type a new size.
        replace_text(stage, "64 x 32");
        key(stage, Key::RETURN);
        assert_eq!(size(seen), "64 x 32");
        assert_eq!(status(seen), "Resized to 64 x 32");
        assert!(seen.borrow().can_undo);

        stage.emit(Msg::Undo);
        assert_eq!(size(seen), "320 x 240");
        stage.emit(Msg::Redo);
        assert_eq!(size(seen), "64 x 32");

        // Reopening shows the current size and Enter keeps it.
        ask_size(stage);
        key(stage, Key::RETURN);
        assert_eq!(status(seen), "Resized to 64 x 32");
        assert_eq!(size(seen), "64 x 32");
    });
}

#[test]
fn a_bad_resize_text_reports_and_changes_nothing() {
    let disk = Rc::new(Disk::default());
    session(&disk, None, |stage, seen, _canvas| {
        for bad in ["abc", "0x10", "20000x20000", "16384x16384", "10"] {
            ask_size(stage);
            replace_text(stage, bad);
            key(stage, Key::RETURN);
            assert!(status(seen).starts_with("Resize failed"), "{bad}");
            assert_eq!(size(seen), "320 x 240", "{bad}");
            assert!(!seen.borrow().can_undo, "{bad}");
        }
    });
}

#[test]
fn cancelling_resize_changes_nothing() {
    let disk = Rc::new(Disk::default());
    session(&disk, None, |stage, seen, _canvas| {
        ask_size(stage);
        replace_text(stage, "10x10");
        key(stage, Key::ESCAPE);
        assert_eq!(size(seen), "320 x 240");
        assert_eq!(status(seen), "");
    });
}
