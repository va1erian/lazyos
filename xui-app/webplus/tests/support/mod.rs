#![allow(dead_code)]
//! The rig of the window tests: the LazyWebPlus window driven offscreen against a recording engine: the engine's news reaches
//! the chrome, the pointer, wheel and keyboard reach the engine, and what a page may start (another
//! app, a command, a download, a dialog) goes where the policy and the window say. Snapshots go to
//! `xui-app/target/snapshots/lazywebplus-*.png` for a human to look at.

use std::cell::{Cell, RefCell};
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::backend::Event as UiEvent;
use xui_core::{Dip, Image, Key, Modifiers, Theme};
use xui_webkit::engine::{Call, Recorder};
use xui_webkit::events::Event;
use xui_webkit::mailbox::{Mailbox, PageState};
use xui_webplus::app::{Browser, Host, Msg, Setup};
use xui_webplus::blocker::Blocker;
use xui_webplus::handoff::Launcher;
use xui_webplus::internal::Internal;
use xui_webplus::notify::{Permissions, ShellNews, Toasts};
use xui_webplus::transfers::{Toast, Transfers};
use xui_webplus::visits::Visits;

pub const SIZE: (i32, i32) = (900, 560);

pub fn register_fonts() {
    let fonts: [&[u8]; 2] = [
        include_bytes!("../../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../../assets/fonts/DroidSans-Bold.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
    xui_canvas::set_default_family("Droid Sans");
}

pub fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/snapshots");
    fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

/// A folder for one test's files, empty.
pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lazywebplus-win-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A page-sized picture: white with a red block and a blue block.
pub fn test_page(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = vec![255u8; (width * height * 4) as usize];
    let mut fill = |x0: u32, y0: u32, x1: u32, y1: u32, rgb: [u8; 3]| {
        for y in y0..y1.min(height) {
            for x in x0..x1.min(width) {
                let at = ((y * width + x) * 4) as usize;
                rgba[at..at + 3].copy_from_slice(&rgb);
            }
        }
    };
    fill(20, 20, 220, 80, [220, 30, 30]);
    fill(20, 100, 220, 160, [30, 60, 220]);
    rgba
}

/// The programs the window started.
#[derive(Default)]
pub struct Opened {
    pub urls: RefCell<Vec<String>>,
    pub paths: RefCell<Vec<String>>,
}

pub struct RecordingLauncher(Rc<Opened>);

impl Launcher for RecordingLauncher {
    fn open_url(&self, url: &str) -> Result<(), String> {
        self.0.urls.borrow_mut().push(url.to_owned());
        Ok(())
    }
    fn open_path(&self, path: &str) -> Result<(), String> {
        self.0.paths.borrow_mut().push(path.to_owned());
        Ok(())
    }
}

/// The shell's toasts, answered at once: what was posted, what was closed, and the news the
/// shell would send next.
#[derive(Default)]
pub struct FakeToasts {
    pub posted: RefCell<Vec<(u64, Toast)>>,
    pub closed: RefCell<Vec<u64>>,
    news: RefCell<Vec<ShellNews>>,
    next: Cell<u64>,
    /// The shell refuses every post (no shell, rate limited).
    pub refuse: Cell<bool>,
}

impl Toasts for FakeToasts {
    fn post(&self, key: u64, toast: Toast) {
        if self.refuse.get() {
            self.news.borrow_mut().push(ShellNews::Failed { key });
            return;
        }
        // An update keeps its toast's id, as the shell does.
        let id = toast.replaces.unwrap_or_else(|| {
            self.next.set(self.next.get() + 1);
            100 + self.next.get()
        });
        self.posted.borrow_mut().push((key, toast));
        self.news
            .borrow_mut()
            .push(ShellNews::Posted { key, toast: id });
    }

    fn close(&self, toast: u64) {
        self.closed.borrow_mut().push(toast);
    }

    fn drain(&self) -> Vec<ShellNews> {
        std::mem::take(&mut *self.news.borrow_mut())
    }
}

impl FakeToasts {
    /// The person clicks toast `toast`.
    pub fn click(&self, toast: u64) {
        self.news.borrow_mut().push(ShellNews::Activated { toast });
    }

    /// Toast `toast` goes away for `reason`.
    pub fn end(&self, toast: u64, reason: xui_webplus::notify::Reason) {
        self.news
            .borrow_mut()
            .push(ShellNews::Closed { toast, reason });
    }
}

pub struct Rig {
    pub engine: Rc<Recorder>,
    pub toasts: Rc<FakeToasts>,
    pub permissions: Permissions,
    pub mailbox: Arc<Mailbox>,
    pub opened: Rc<Opened>,
    pub internal: Internal,
    pub blocker: Blocker,
    pub home: PathBuf,
    /// Where the window said its controls are (`Host::probe`), by name.
    pub rects: Rc<RefCell<std::collections::HashMap<String, xui_core::geometry::Rect>>>,
}

impl Rig {
    pub fn downloads(&self) -> PathBuf {
        self.home.join("Downloads")
    }

    pub fn history_file(&self) -> PathBuf {
        self.home.join("history.tsv")
    }

    /// The middle of the control the window named, in client pixels.
    pub fn centre(&self, name: &str) -> (i32, i32) {
        let r = self.rects.borrow()[name];
        ((r.left + r.right) / 2, (r.top + r.bottom) / 2)
    }

    pub fn bookmarks_file(&self) -> PathBuf {
        self.home.join("bookmarks.log")
    }

    pub fn notify_file(&self) -> PathBuf {
        self.home.join("notify.tsv")
    }

    /// Has the window take what the shell reported (the toasts the fake answered at once).
    pub fn shell(&self, stage: &Stage<'_, Msg>) {
        stage.emit(Msg::Shell);
    }

    /// Posts a page state (what the engine reports) and has the window take it.
    pub fn state(&self, stage: &Stage<'_, Msg>, state: PageState) {
        self.mailbox.post_state(state);
        stage.emit(Msg::State);
    }

    pub fn events(&self, stage: &Stage<'_, Msg>, events: Vec<Event>) {
        for event in events {
            self.mailbox.post_event(event);
        }
        stage.emit(Msg::Events);
    }
}

pub fn drive(
    name: &str,
    first: Option<&str>,
    step: impl FnOnce(&Stage<'_, Msg>, &Rig) + 'static,
) -> (Image, Vec<Call>) {
    let (image, calls, _) = drive_full(name, first, step);
    (image, calls)
}

pub fn drive_full(
    name: &str,
    first: Option<&str>,
    step: impl FnOnce(&Stage<'_, Msg>, &Rig) + 'static,
) -> (Image, Vec<Call>, Rc<Opened>) {
    drive_with(name, first, |_| {}, |host, _| host, step)
}

/// Like [`drive_full`], with the test's own folder seeded by `prepare` before the window opens and
/// the host (profile folder, prefs) adjusted by `tweak`, which is given that folder.
pub fn drive_with(
    name: &str,
    first: Option<&str>,
    prepare: impl FnOnce(&std::path::Path),
    tweak: impl FnOnce(Host, &std::path::Path) -> Host + 'static,
    step: impl FnOnce(&Stage<'_, Msg>, &Rig) + 'static,
) -> (Image, Vec<Call>, Rc<Opened>) {
    register_fonts();
    let home = scratch(name);
    prepare(&home);
    let engine = Rc::new(Recorder::default());
    let mailbox = Arc::new(Mailbox::default());
    let opened = Rc::new(Opened::default());
    let internal = Internal::default();
    let blocker = Blocker::volatile();
    let started = Rc::clone(&opened);
    let toasts = Rc::new(FakeToasts::default());
    let permissions = Permissions::open(home.join("notify.tsv"));
    let rects: Rc<RefCell<std::collections::HashMap<String, xui_core::geometry::Rect>>> =
        Rc::default();
    let probe_rects = Rc::clone(&rects);
    let rig = Rig {
        toasts: Rc::clone(&toasts),
        permissions: permissions.clone(),
        engine: Rc::clone(&engine),
        mailbox: Arc::clone(&mailbox),
        opened: Rc::clone(&opened),
        internal: internal.clone(),
        blocker: blocker.clone(),
        home: home.clone(),
        rects: Rc::clone(&rects),
    };
    let first = first.map(str::to_owned);
    let profile = home.clone();
    let (build_engine, build_mailbox) = (Rc::clone(&engine), Arc::clone(&mailbox));
    let image = render_with(
        Snapshot::new(Dip(SIZE.0 as f32), Dip(SIZE.1 as f32)).theme(Theme::light()),
        move |ui| {
            Browser::build(
                ui,
                build_engine,
                build_mailbox,
                Setup {
                    url: first,
                    visits: Visits::open(home.join("history.tsv")),
                    transfers: Transfers::new(home.join("Downloads"), None),
                    internal,
                    permissions,
                    toasts: Some(toasts as Rc<dyn Toasts>),
                    blocker,
                    host: {
                        let host = Host {
                            launcher: Rc::new(RecordingLauncher(opened)),
                            profile: Some(profile),
                            probe: Box::new(move |_, name, rect| {
                                probe_rects.borrow_mut().insert(name.to_owned(), rect);
                            }),
                            ..Host::none()
                        };
                        tweak(host, &home)
                    },
                },
            )
        },
        move |stage| step(stage, &rig),
    )
    .expect("the headless render");
    (image, engine.calls(), started)
}

/// The state of a page that finished loading.
pub fn loaded(url: &str, title: &str, finished: u32) -> PageState {
    PageState {
        url: url.into(),
        committed_url: url.into(),
        title: title.into(),
        secure: url.starts_with("https://"),
        progress: 1000,
        finished,
        ..PageState::default()
    }
}

pub fn loading(url: &str) -> PageState {
    PageState {
        url: url.into(),
        loading: true,
        progress: 300,
        ..PageState::default()
    }
}

pub fn page_origin(stage: &Stage<'_, Msg>) -> (i32, i32) {
    // Below the menu bar (28) and the toolbar (38).
    (0, Dip(66.0).to_px(stage.dpi()).0)
}

pub fn key_down(key: Key) -> UiEvent {
    UiEvent::KeyDown {
        key,
        modifiers: Modifiers::NONE,
        repeat: 1,
        system: false,
    }
}

/// The bytes of a `data:` URL the window built (percent-encoded after the first comma).
pub fn support_decode(url: &str) -> Vec<u8> {
    let body = url.split_once(',').map_or("", |(_, rest)| rest).as_bytes();
    let mut out = Vec::with_capacity(body.len());
    let mut i = 0;
    while i < body.len() {
        if body[i] == b'%' && i + 2 < body.len() + 0 {
            if let Ok(hex) = u8::from_str_radix(&String::from_utf8_lossy(&body[i + 1..i + 3]), 16) {
                out.push(hex);
                i += 3;
                continue;
            }
        }
        out.push(body[i]);
        i += 1;
    }
    out
}
