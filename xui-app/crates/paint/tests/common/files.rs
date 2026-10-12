//! A path-aware in-memory store and a probed session over it, for the tests
//! of the Image and Adjustments dialogs and the file flows: the app is built
//! with the Open/Save pickers over a fake filesystem, a probe that reports
//! every named control, and a reporter that records `PAINT:OPEN`/`PAINT:SAVE`.

#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use xui_canvas::snapshot::{Snapshot, Stage, render_with};
use xui_core::Dip;
use xui_core::Theme;
use xui_core::backend::Event;
use xui_core::geometry::Rect;
use xui_core::image::Image;
use xui_core::message::{Key, Modifiers, MouseButton};
use xui_core::widget::{Entry, FileSystem, TaskDialogAction};
use xui_paint::Msg;
use xui_paint::storage::Storage;
use xui_paint::view::{Observer, PaintApp, Report};

/// Files by absolute path, shared by the fake filesystem and the store.
#[derive(Default)]
pub struct Disk {
    pub files: RefCell<BTreeMap<PathBuf, Vec<u8>>>,
    pub fail_writes: Cell<bool>,
}

impl Disk {
    pub fn with(files: &[(&str, Vec<u8>)]) -> Rc<Disk> {
        let disk = Rc::new(Disk::default());
        for (path, bytes) in files {
            disk.files.borrow_mut().insert(path.into(), bytes.clone());
        }
        disk
    }

    pub fn get(&self, path: &str) -> Option<Vec<u8>> {
        self.files.borrow().get(Path::new(path)).cloned()
    }
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

struct DiskStorage(Rc<Disk>);

impl Storage for DiskStorage {
    fn save(&self, _bytes: &[u8]) -> Result<(), String> {
        Err("path-less save is not used with dialogs".to_string())
    }

    fn load(&self) -> Option<Vec<u8>> {
        None
    }

    fn available(&self) -> bool {
        true
    }

    fn save_to(&self, path: &Path, bytes: &[u8]) -> Result<(), String> {
        if self.0.fail_writes.get() {
            return Err("disk full".to_string());
        }
        self.0
            .files
            .borrow_mut()
            .insert(path.to_path_buf(), bytes.to_vec());
        Ok(())
    }

    fn load_from(&self, path: &Path) -> Option<Vec<u8>> {
        self.0.files.borrow().get(path).cloned()
    }

    fn supports_paths(&self) -> bool {
        true
    }
}

pub type Stg<'a> = Stage<'a, Msg>;

/// What a step reads: the observer, the probed rectangles, the disk and the
/// reports the app made.
pub struct Ctx {
    pub observer: Rc<RefCell<Observer>>,
    pub names: Rc<RefCell<BTreeMap<String, Rect>>>,
    pub disk: Rc<Disk>,
    pub reports: Rc<RefCell<Vec<Report>>>,
}

impl Ctx {
    pub fn rect(&self, name: &str) -> Rect {
        *self
            .names
            .borrow()
            .get(name)
            .unwrap_or_else(|| panic!("{name} probed"))
    }

    pub fn has(&self, name: &str) -> bool {
        self.names.borrow().contains_key(name)
    }

    pub fn centre(&self, name: &str) -> (i32, i32) {
        let r = self.rect(name);
        ((r.left + r.right) / 2, (r.top + r.bottom) / 2)
    }

    pub fn status(&self) -> String {
        self.observer.borrow().status[3].clone()
    }

    pub fn size(&self) -> String {
        self.observer.borrow().status[1].clone()
    }

    pub fn title(&self) -> String {
        self.observer.borrow().title.clone()
    }

    pub fn dirty(&self) -> bool {
        self.observer.borrow().dirty
    }

    pub fn history(&self) -> (usize, usize) {
        self.observer.borrow().history
    }

    pub fn layers(&self) -> usize {
        self.observer.borrow().layers.len()
    }

    /// Probes again (the controls move with the layout) and forgets the
    /// controls of dialogs that have closed.
    pub fn probe(&self, stage: &Stg<'_>) {
        self.names.borrow_mut().retain(|name, _| {
            !(name.starts_with("resize_")
                || name.starts_with("canvas_")
                || name.starts_with("anchor:")
                || name.starts_with("adjust_"))
        });
        stage.emit(Msg::Probe);
    }
}

pub fn click_at(stage: &Stg<'_>, at: (i32, i32)) {
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

/// Probes again and clicks the centre of `name`.
pub fn click(stage: &Stg<'_>, ctx: &Ctx, name: &str) {
    ctx.probe(stage);
    click_at(stage, ctx.centre(name));
}

/// Clicks `name` at `fraction` (0 to 1) of its width.
pub fn click_along(stage: &Stg<'_>, ctx: &Ctx, name: &str, fraction: f32) {
    ctx.probe(stage);
    let r = ctx.rect(name);
    let x = r.left + (r.width() as f32 * fraction) as i32;
    click_at(stage, (x, (r.top + r.bottom) / 2));
}

pub fn key(stage: &Stg<'_>, key: Key) {
    key_with(stage, key, Modifiers::NONE);
}

pub fn key_with(stage: &Stg<'_>, key: Key, modifiers: Modifiers) {
    stage.inject(Event::KeyDown {
        key,
        modifiers,
        repeat: 1,
        system: false,
    });
}

pub fn chord(stage: &Stg<'_>, key: Key, shift: bool) {
    key_with(
        stage,
        key,
        Modifiers {
            ctrl: true,
            shift,
            ..Modifiers::NONE
        },
    );
}

/// Replaces the text of the field `name` by clicking it, selecting all and
/// typing. (The offscreen backend records focus but never delivers
/// `SetFocus`, which an `Edit` needs before it accepts typing.)
pub fn type_into(stage: &Stg<'_>, ctx: &Ctx, name: &str, text: &str) {
    click(stage, ctx, name);
    stage.inject(Event::SetFocus);
    chord(stage, Key::A, false);
    if text.is_empty() {
        key(stage, Key::DELETE);
    }
    for c in text.chars() {
        stage.inject(Event::Char(c));
    }
}

/// Replaces the focused picker field text: select all, then type.
pub fn replace_text(stage: &Stg<'_>, text: &str) {
    stage.inject(Event::SetFocus);
    chord(stage, Key::A, false);
    type_text(stage, text);
}

/// Types into the focused picker field.
pub fn type_text(stage: &Stg<'_>, text: &str) {
    stage.inject(Event::SetFocus);
    for c in text.chars() {
        stage.inject(Event::Char(c));
    }
}

pub fn dont_save(stage: &Stg<'_>) {
    stage.emit(Msg::Unsaved(TaskDialogAction::Command(1)));
}

/// A drag on the canvas from picture pixel `from` to `to` at 100%.
pub fn drag_picture(stage: &Stg<'_>, ctx: &Ctx, from: (i32, i32), to: (i32, i32)) {
    ctx.probe(stage);
    let canvas = ctx.rect("canvas");
    let mut viewport = xui_paint::viewport::Viewport::new();
    viewport.clamp((320, 240), (canvas.width(), canvas.height()));
    let place = |(x, y): (i32, i32)| {
        let (vx, vy) = viewport.to_view(x, y);
        (canvas.left + vx, canvas.top + vy)
    };
    let (a, b) = (place(from), place(to));
    stage.inject(Event::MouseDown {
        x: a.0,
        y: a.1,
        button: MouseButton::Left,
        modifiers: Modifiers::NONE,
    });
    stage.inject(Event::MouseMove {
        x: b.0,
        y: b.1,
        modifiers: Modifiers::NONE,
    });
    stage.inject(Event::MouseUp {
        x: b.0,
        y: b.1,
        button: MouseButton::Left,
        modifiers: Modifiers::NONE,
    });
}

/// The window position of picture pixel `(x, y)` of the default picture.
pub fn picture_at(ctx: &Ctx, x: i32, y: i32) -> (u32, u32) {
    let canvas = ctx.rect("canvas");
    let mut viewport = xui_paint::viewport::Viewport::new();
    viewport.clamp((320, 240), (canvas.width(), canvas.height()));
    let (vx, vy) = viewport.to_view(x, y);
    ((canvas.left + vx) as u32, (canvas.top + vy) as u32)
}

/// Runs `step` against the app over `disk`, in `theme` at `dpi`; the image
/// is the window as the step left it.
pub fn session_at(
    disk: &Rc<Disk>,
    theme: Theme,
    dpi: u32,
    step: impl FnOnce(&Stg<'_>, &Ctx) + 'static,
) -> Image {
    let observer = Rc::new(RefCell::new(Observer::default()));
    let build_observer = Rc::clone(&observer);
    let names: Rc<RefCell<BTreeMap<String, Rect>>> = Rc::new(RefCell::new(BTreeMap::new()));
    let hook_names = Rc::clone(&names);
    let reports: Rc<RefCell<Vec<Report>>> = Rc::new(RefCell::new(Vec::new()));
    let hook_reports = Rc::clone(&reports);
    let storage = Rc::new(DiskStorage(Rc::clone(disk)));
    let fs: Rc<dyn FileSystem> = Rc::new(FakeFs(Rc::clone(disk)));
    let disk = Rc::clone(disk);
    render_with(
        Snapshot::new(Dip(800.0), Dip(600.0)).theme(theme).dpi(dpi),
        move |ui| {
            let mut app = PaintApp::build_with_files_observed(ui, storage, fs, build_observer)?;
            app.set_start_dir("/");
            app.set_probe(move |_, name, rect| {
                hook_names.borrow_mut().insert(name.to_string(), rect);
            });
            app.set_reporter(move |report| hook_reports.borrow_mut().push(report.clone()));
            Ok(app)
        },
        move |stage| {
            stage.emit(Msg::Probe);
            step(
                stage,
                &Ctx {
                    observer,
                    names,
                    disk,
                    reports,
                },
            )
        },
    )
    .expect("render")
}

pub fn session(disk: &Rc<Disk>, step: impl FnOnce(&Stg<'_>, &Ctx) + 'static) -> Image {
    session_at(disk, Theme::light(), 96, step)
}

/// Like [`session`], and also the window positions, in the final frame, of
/// the picture pixels `picks` of the default 320 x 240 picture.
pub fn session_pixels(
    disk: &Rc<Disk>,
    picks: &'static [(i32, i32)],
    step: impl FnOnce(&Stg<'_>, &Ctx) + 'static,
) -> (Image, Vec<(u32, u32)>) {
    let found = Rc::new(RefCell::new(Vec::new()));
    let out = Rc::clone(&found);
    let image = session(disk, move |stage, ctx| {
        step(stage, ctx);
        ctx.probe(stage);
        *out.borrow_mut() = picks.iter().map(|&(x, y)| picture_at(ctx, x, y)).collect();
    });
    let positions = found.borrow().clone();
    (image, positions)
}

/// The colour of window pixel `at` in `image`, without alpha.
pub fn rgb(image: &Image, at: (u32, u32)) -> [u8; 3] {
    let p = image.pixel(at.0, at.1).expect("inside the frame");
    [p[0], p[1], p[2]]
}
