//! The Archiver window driven offscreen: open, browse, create from a drop,
//! extract, delete, the drag bridge, and snapshots of the window
//! (`target/snapshots/archiver-*.png`) for a human to look at.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use xui_archiver::{ArchiverApp, DragState, Host, Msg, WINDOW};
use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::backend::BackendError;
use xui_core::widget::{DialogAction, TaskDialogAction};
use xui_core::{Dip, Image, Theme};

/// A unique folder under the temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("archiver-ui-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../archive/tests/fixtures")
        .join(name)
}

fn register_fonts() {
    let fonts: [&[u8]; 2] = [
        include_bytes!("../../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../../assets/fonts/DroidSans-Bold.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
    xui_canvas::set_default_family("Droid Sans");
}

/// Run `test` on its own thread with the fonts, failing if it hangs.
fn watchdog<T: Send + 'static>(test: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        register_fonts();
        let _ = tx.send(test());
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(value) => {
            let _ = handle.join();
            value
        }
        Err(_) => match handle.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => panic!("the window test hung"),
        },
    }
}

/// Render the window after `step` drives it; `step` gets the drag bridge
/// (which mirrors the app's state) and records what it saw.
fn drive(
    dir: PathBuf,
    theme: Theme,
    step: impl FnOnce(&Stage<'_, Msg>, &Rc<RefCell<DragState>>, &Rc<RefCell<Vec<String>>>) + 'static,
) -> (Image, Vec<String>) {
    let bridge: Rc<RefCell<DragState>> = Rc::default();
    let log: Rc<RefCell<Vec<String>>> = Rc::default();
    let (app_bridge, app_log, step_bridge, step_log) = (
        Rc::clone(&bridge),
        Rc::clone(&log),
        Rc::clone(&bridge),
        Rc::clone(&log),
    );
    let image = render_with(
        Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)).theme(theme),
        move |ui| {
            let mut host = Host::std(&dir);
            host.temp_dir = dir.join(".scratch");
            let log = Rc::clone(&app_log);
            host.log = Rc::new(move |line| log.borrow_mut().push(line.to_owned()));
            ArchiverApp::build(ui, host, app_bridge).map_err(|e: BackendError| e)
        },
        move |stage| step(stage, &step_bridge, &step_log),
    )
    .expect("the headless render");
    let seen = log.borrow().clone();
    (image, seen)
}

/// Pump ticks until the running job is done.
fn wait(stage: &Stage<'_, Msg>, bridge: &Rc<RefCell<DragState>>) {
    let started = Instant::now();
    loop {
        stage.emit(Msg::Tick);
        if !bridge.borrow().busy {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the job never finished"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn names(bridge: &Rc<RefCell<DragState>>) -> Vec<String> {
    bridge
        .borrow()
        .rows
        .iter()
        .map(|row| row.name.clone())
        .collect()
}

fn index_of(bridge: &Rc<RefCell<DragState>>, name: &str) -> usize {
    bridge
        .borrow()
        .rows
        .iter()
        .position(|row| row.name == name)
        .unwrap()
}

fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

#[test]
fn opening_and_browsing_a_7zip_archive() {
    let (image, log) = watchdog(|| {
        let dir = TempDir::new("browse");
        let result = drive(dir.0.clone(), Theme::light(), |stage, bridge, seen| {
            stage.emit(Msg::OpenChosen(fixture("7zip.zip")));
            wait(stage, bridge);
            seen.borrow_mut().push(format!("root={:?}", names(bridge)));
            stage.emit(Msg::Activate(index_of(bridge, "tree")));
            seen.borrow_mut().push(format!("tree={:?}", names(bridge)));
            stage.emit(Msg::Activate(index_of(bridge, "sub")));
            stage.emit(Msg::Selection(vec![1]));
            seen.borrow_mut().push(format!("sub={:?}", names(bridge)));
            stage.emit(Msg::Up);
            seen.borrow_mut()
                .push(format!("up={}", bridge.borrow().folder));
        });
        drop(dir);
        result
    });
    save(&image, "archiver-browse.png");
    assert!(
        log.iter().any(|l| l == "ARCHIVER:OPEN:PASS:ZIP:5"),
        "{log:?}"
    );
    let seen: Vec<&String> = log.iter().filter(|l| !l.starts_with("ARCHIVER:")).collect();
    assert_eq!(
        seen,
        [
            "root=[\"tree\"]",
            "tree=[\"..\", \"sub\", \"empty.txt\", \"hello.txt\"]",
            "sub=[\"..\", \"numbers.txt\"]",
            "up=tree",
        ]
    );
}

#[test]
fn dropping_files_with_no_archive_open_makes_a_new_one() {
    let (image, log) = watchdog(|| {
        let dir = TempDir::new("create");
        std::fs::create_dir_all(dir.0.join("photos")).unwrap();
        std::fs::write(dir.0.join("photos/a.txt"), "alpha").unwrap();
        std::fs::write(dir.0.join("notes.md"), "# notes").unwrap();
        let target = dir.0.join("bundle.tar.gz");
        let sources = vec![dir.0.join("photos"), dir.0.join("notes.md")];
        let result = drive(dir.0.clone(), Theme::dark(), move |stage, bridge, seen| {
            stage.emit(Msg::DragEnter);
            stage.emit(Msg::Dropped(sources));
            // The New picker is open; choose the name.
            stage.emit(Msg::NewChosen(target.clone()));
            wait(stage, bridge);
            seen.borrow_mut().push(format!("rows={:?}", names(bridge)));
            seen.borrow_mut()
                .push(format!("exists={}", target.is_file()));
        });
        drop(dir);
        result
    });
    save(&image, "archiver-created-dark.png");
    assert!(
        log.iter().any(|l| l == "ARCHIVER:CREATED:PASS:2"),
        "{log:?}"
    );
    assert!(
        log.contains(&"rows=[\"photos\", \"notes.md\"]".to_owned()),
        "{log:?}"
    );
    assert!(log.contains(&"exists=true".to_owned()));
}

#[test]
fn extracting_a_folder_and_deleting_from_a_copy() {
    let log = watchdog(|| {
        let dir = TempDir::new("extract");
        let copy = dir.0.join("copy.zip");
        std::fs::copy(fixture("7zip.zip"), &copy).unwrap();
        let out = dir.0.join("out");
        let (out_check, dir_path) = (out.clone(), dir.0.clone());
        let (_, log) = drive(dir_path, Theme::light(), move |stage, bridge, seen| {
            stage.emit(Msg::OpenChosen(copy));
            wait(stage, bridge);
            stage.emit(Msg::Activate(index_of(bridge, "tree")));
            stage.emit(Msg::Selection(vec![index_of(bridge, "sub")]));
            stage.emit(Msg::Extract);
            stage.emit(Msg::ExtractTo(DialogAction::Accept(
                out_check.to_string_lossy().into_owned(),
            )));
            wait(stage, bridge);
            seen.borrow_mut().push(format!(
                "extracted={}",
                out_check.join("sub/numbers.txt").is_file()
            ));
            stage.emit(Msg::Selection(vec![index_of(bridge, "hello.txt")]));
            stage.emit(Msg::Delete);
            stage.emit(Msg::DeleteConfirmed(TaskDialogAction::Command(0)));
            wait(stage, bridge);
            seen.borrow_mut().push(format!("after={:?}", names(bridge)));
        });
        drop(dir);
        log
    });
    assert!(log.contains(&"extracted=true".to_owned()), "{log:?}");
    assert!(
        log.iter().any(|l| l.starts_with("ARCHIVER:DELETED:PASS")),
        "{log:?}"
    );
    assert!(
        log.contains(&"after=[\"..\", \"sub\", \"empty.txt\"]".to_owned()),
        "{log:?}"
    );
}

#[test]
fn the_drag_bridge_extracts_what_a_drag_carries() {
    let log = watchdog(|| {
        let dir = TempDir::new("drag");
        let scratch = dir.0.join("scratch");
        let (_, log) = drive(dir.0.clone(), Theme::light(), move |stage, bridge, seen| {
            stage.emit(Msg::OpenChosen(fixture("lzma2.7z")));
            wait(stage, bridge);
            stage.emit(Msg::Activate(index_of(bridge, "tree")));
            let (sub, hello) = (index_of(bridge, "sub"), index_of(bridge, "hello.txt"));
            stage.emit(Msg::Selection(vec![sub, hello]));
            // The press that starts the drag collapses the selection.
            stage.emit(Msg::Selection(vec![hello]));
            let list = bridge.borrow().list.unwrap();
            seen.borrow_mut()
                .push(format!("can={}", bridge.borrow().can_drag_from(list, 40)));
            let paths = xui_archiver::drag::prepare(&bridge.borrow(), &scratch).unwrap();
            let names: Vec<String> = paths
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            seen.borrow_mut().push(format!("paths={names:?}"));
            seen.borrow_mut()
                .push(format!("data={}", paths.iter().all(|p| p.exists())));
            seen.borrow_mut().push(format!(
                "numbers={}",
                Path::new(&paths[1]).join("numbers.txt").is_file()
            ));
        });
        drop(dir);
        log
    });
    assert!(log.contains(&"can=true".to_owned()), "{log:?}");
    assert!(
        log.contains(&"paths=[\"hello.txt\", \"sub\"]".to_owned()),
        "{log:?}"
    );
    assert!(log.contains(&"data=true".to_owned()));
    assert!(log.contains(&"numbers=true".to_owned()));
}

#[test]
fn the_empty_window_invites_a_drop() {
    let (image, _) = watchdog(|| {
        let dir = TempDir::new("empty");
        let result = drive(dir.0.clone(), Theme::light(), |_, _, _| {});
        drop(dir);
        result
    });
    save(&image, "archiver-empty.png");
}
