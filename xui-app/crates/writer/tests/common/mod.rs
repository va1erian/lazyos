//! The headless rig the window tests share: the bundled fonts, a temp
//! folder, the whole LazyWriter window rendered offscreen, and a watchdog.
#![allow(dead_code)]

pub mod printer;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use xui_canvas::snapshot::{Snapshot, Stage, render_with};
use xui_core::backend::BackendError;
use xui_core::widget::{Edit, Label, StatusBar, ToggleButton};
use xui_core::{Dip, Image, Theme};
use xui_rich_text::RichTextEditor;
use xui_writer::{Host, Msg};

/// The window size the LazyOS binary asks for.
pub const WIDTH: f32 = 960.0;
pub const HEIGHT: f32 = 600.0;

/// How long one window test may run before the watchdog fails it.
const WATCHDOG: Duration = Duration::from_secs(120);

/// Registers the fonts LazyOS bundles, as `xui_app::font::register_writer`
/// does, so the snapshots match the guest. The shaper is per thread.
fn register_fonts() {
    let fonts: [&[u8]; 4] = [
        include_bytes!("../../../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../../../assets/fonts/DroidSans-Bold.ttf"),
        include_bytes!("../../../../../assets/fonts/DroidSerif-Regular.ttf"),
        include_bytes!("../../../../../assets/fonts/JetBrainsMono-Regular.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
    xui_canvas::set_default_family("Droid Sans");
}

/// Runs `test` on its own thread (with the fonts registered) and fails it if
/// it does not finish within the watchdog's time. A message-loop test that
/// hangs fails instead of stalling the suite.
pub fn watchdog<T: Send + 'static>(test: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    let handle = std::thread::spawn(move || {
        register_fonts();
        let _ = tx.send(test());
    });
    match rx.recv_timeout(WATCHDOG) {
        Ok(value) => {
            let _ = handle.join();
            value
        }
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("the window test hung"),
        Err(mpsc::RecvTimeoutError::Disconnected) => match handle.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => panic!("the window test ended without a result"),
        },
    }
}

/// A unique folder under the system temp dir, removed on drop.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("lazywriter-ui-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    pub fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Handles on the live window's widgets, cloned out of the app as it is built.
#[derive(Clone)]
pub struct Rig {
    pub editor: Rc<RichTextEditor<Msg>>,
    pub status: Rc<StatusBar<Msg>>,
    pub bold: Rc<ToggleButton<Msg>>,
    pub dialog_open: Rc<Cell<bool>>,
    pub print_printer: Rc<Edit<Msg>>,
    pub print_status: Rc<Label<Msg>>,
}

impl Rig {
    pub fn status(&self, part: usize) -> String {
        self.status.text(part).unwrap_or_default()
    }

    pub fn text(&self) -> String {
        self.editor.with_document(|d| d.to_plain_text())
    }
}

/// Lets queued messages (from `RichTextEditor::exec`) reach the app.
pub fn pump(stage: &Stage<'_, Msg>) {
    // An empty spot of the document, so no toolbar item shows a hover.
    stage.hover(500, 500);
}

/// A host over `dir` with the bundled families.
pub fn host(dir: &std::path::Path) -> Host {
    let mut host = Host::std(dir);
    host.serif_family = "Droid Serif".into();
    host.mono_family = "JetBrains Mono".into();
    host
}

/// Renders the whole window in `theme` after `step` drives it.
pub fn render(
    theme: Theme,
    dir: PathBuf,
    step: impl FnOnce(&Stage<'_, Msg>, &Rig) + 'static,
) -> Image {
    let slot: Rc<RefCell<Option<Rig>>> = Rc::default();
    let built = Rc::clone(&slot);
    render_with(
        Snapshot::new(Dip(WIDTH), Dip(HEIGHT)).theme(theme),
        move |ui| {
            let app = xui_writer::ui::build(ui, host(&dir))?;
            *built.borrow_mut() = Some(Rig {
                editor: Rc::clone(&app.editor),
                status: Rc::clone(&app.status),
                bold: Rc::clone(&app.tools.marks[0]),
                dialog_open: Rc::clone(&app.dialog_open),
                print_printer: Rc::clone(&app.print_bar.printer),
                print_status: Rc::clone(&app.print_bar.status),
            });
            Ok::<_, BackendError>(app)
        },
        move |stage| {
            let rig = slot.borrow().clone().expect("the window was built");
            step(stage, &rig);
        },
    )
    .expect("the headless render")
}

/// Where the snapshots are saved for a human to look at.
pub fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).expect("snapshot dir");
    image.save_png(dir.join(name)).expect("save snapshot");
}
