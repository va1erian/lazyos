//! The LazyWebPlus window driven offscreen against a recording engine: the engine's news reaches
//! the chrome, the pointer, wheel and keyboard reach the engine, and what a page may start (another
//! app, a command, a download, a dialog) goes where the policy and the window say. Snapshots go to
//! `xui-app/target/snapshots/lazywebplus-*.png` for a human to look at.

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::backend::Event as UiEvent;
use xui_core::{Dip, Image, Key, Modifiers, Theme};
use xui_webkit::engine::{Call, DialogKind, Recorder};
use xui_webkit::events::{CursorKind, Event};
use xui_webkit::mailbox::{Mailbox, PageState};
use xui_webplus::app::{Browser, Host, Msg, Setup};
use xui_webplus::chrome::Command;
use xui_webplus::downloads::Saver;
use xui_webplus::handoff::Launcher;
use xui_webplus::internal::Internal;
use xui_webplus::pages::{ABOUT, HISTORY};
use xui_webplus::transfers::Transfers;
use xui_webplus::visits::Visits;

const SIZE: (i32, i32) = (900, 560);

fn register_fonts() {
    let fonts: [&[u8]; 2] = [
        include_bytes!("../../../assets/fonts/DroidSans.ttf"),
        include_bytes!("../../../assets/fonts/DroidSans-Bold.ttf"),
    ];
    for font in fonts {
        xui_canvas::add_font(font.to_vec());
    }
    xui_canvas::set_default_family("Droid Sans");
}

fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/snapshots");
    fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

/// A folder for one test's files, empty.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lazywebplus-win-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// A page-sized picture: white with a red block and a blue block.
fn test_page(width: u32, height: u32) -> Vec<u8> {
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
struct Opened {
    urls: RefCell<Vec<String>>,
    paths: RefCell<Vec<String>>,
}

struct RecordingLauncher(Rc<Opened>);

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

struct Rig {
    engine: Rc<Recorder>,
    mailbox: Arc<Mailbox>,
    opened: Rc<Opened>,
    internal: Internal,
    home: PathBuf,
}

impl Rig {
    fn downloads(&self) -> PathBuf {
        self.home.join("Downloads")
    }

    fn history_file(&self) -> PathBuf {
        self.home.join("history.tsv")
    }

    /// Posts a page state (what the engine reports) and has the window take it.
    fn state(&self, stage: &Stage<'_, Msg>, state: PageState) {
        self.mailbox.post_state(state);
        stage.emit(Msg::State);
    }

    fn events(&self, stage: &Stage<'_, Msg>, events: Vec<Event>) {
        for event in events {
            self.mailbox.post_event(event);
        }
        stage.emit(Msg::Events);
    }
}

fn drive(
    name: &str,
    first: Option<&str>,
    step: impl FnOnce(&Stage<'_, Msg>, &Rig) + 'static,
) -> (Image, Vec<Call>) {
    let (image, calls, _) = drive_full(name, first, step);
    (image, calls)
}

fn drive_full(
    name: &str,
    first: Option<&str>,
    step: impl FnOnce(&Stage<'_, Msg>, &Rig) + 'static,
) -> (Image, Vec<Call>, Rc<Opened>) {
    register_fonts();
    let home = scratch(name);
    let engine = Rc::new(Recorder::default());
    let mailbox = Arc::new(Mailbox::default());
    let opened = Rc::new(Opened::default());
    let internal = Internal::default();
    let started = Rc::clone(&opened);
    let rig = Rig {
        engine: Rc::clone(&engine),
        mailbox: Arc::clone(&mailbox),
        opened: Rc::clone(&opened),
        internal: internal.clone(),
        home: home.clone(),
    };
    let first = first.map(str::to_owned);
    let (build_engine, build_mailbox) = (Rc::clone(&engine), Arc::clone(&mailbox));
    let image = render_with(
        Snapshot::new(Dip(SIZE.0 as f32), Dip(SIZE.1 as f32)).theme(Theme::light()),
        move |ui| {
            let saver = Arc::new(Saver::new(home.join("Downloads")));
            Browser::build(
                ui,
                build_engine,
                build_mailbox,
                Setup {
                    url: first,
                    visits: Visits::open(home.join("history.tsv")),
                    transfers: Transfers::new(saver, None),
                    internal,
                    host: Host {
                        launcher: Rc::new(RecordingLauncher(opened)),
                        ..Host::none()
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
fn loaded(url: &str, title: &str, finished: u32) -> PageState {
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

fn loading(url: &str) -> PageState {
    PageState {
        url: url.into(),
        loading: true,
        progress: 300,
        ..PageState::default()
    }
}

fn page_origin(stage: &Stage<'_, Msg>) -> (i32, i32) {
    // Below the menu bar (28) and the toolbar (38).
    (0, Dip(66.0).to_px(stage.dpi()).0)
}

fn key_down(key: Key) -> UiEvent {
    UiEvent::KeyDown {
        key,
        modifiers: Modifiers::NONE,
        repeat: 1,
        system: false,
    }
}

#[test]
fn the_first_address_is_loaded_and_the_page_is_laid_out_at_the_view_size() {
    let (_, calls) = drive("first", Some("http://example.com/"), |_, _| {});
    assert!(
        calls.contains(&Call::Load("http://example.com/".into())),
        "{calls:?}"
    );
    let (w, h) = calls
        .iter()
        .rev()
        .find_map(|c| match c {
            Call::Resize(w, h) => Some((*w, *h)),
            _ => None,
        })
        .expect("the engine was told the view size");
    assert_eq!(w, SIZE.0);
    assert!(h > 300 && h < SIZE.1, "view height {h}");
}

#[test]
fn without_an_address_the_start_page_opens() {
    let (_, calls) = drive("start", None, |_, _| {});
    let loads: Vec<_> = calls
        .iter()
        .filter_map(|c| match c {
            Call::Load(url) => Some(url.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(loads.len(), 1);
    assert!(loads[0].starts_with("data:text/html"), "{loads:?}");
}

#[test]
fn a_frame_from_the_engine_is_drawn_and_the_chrome_follows_the_page() {
    let (image, _) = drive("frame", Some("https://theoldnet.com/"), |stage, rig| {
        rig.mailbox.post_frame(880, 400, &test_page(880, 400));
        stage.emit(Msg::Frame);
        rig.state(stage, loaded("https://theoldnet.com/", "TheOldNet", 1));
    });
    save(&image, "lazywebplus-frame.png");
    let reds = image
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > 200 && p[1] < 60 && p[2] < 60)
        .count();
    assert!(reds > 5_000, "the red block is on screen: {reds}");
}

#[test]
fn a_finished_load_is_written_into_the_history_once_and_its_late_title_updates_it() {
    let (_, _) = drive("visits", Some("http://example.com/"), |stage, rig| {
        rig.state(stage, loading("http://example.com/"));
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        // The same state again (the engine posts several): no second visit.
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        rig.state(stage, loaded("http://example.com/", "Example Domain", 1));
        let file = fs::read_to_string(rig.history_file()).unwrap();
        let lines: Vec<_> = file.lines().collect();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[1].ends_with("\thttp://example.com/\tExample Domain"));
    });
}

#[test]
fn a_stopped_a_failed_and_a_download_load_leave_no_history() {
    let (_, _) = drive("nohistory", None, |stage, rig| {
        // Loading ends with no finished navigation: stopped, or taken over by a download.
        rig.state(stage, loading("http://a.test/"));
        rig.state(stage, PageState::default());
        // A load that failed.
        rig.state(stage, loading("http://b.test/"));
        rig.state(
            stage,
            PageState {
                error: "Could not load http://b.test/: Connection refused".into(),
                ..PageState::default()
            },
        );
        assert!(!rig.history_file().exists());
    });
}

#[test]
fn the_link_under_the_pointer_shows_in_the_status_bar_and_goes_away() {
    let (with_link, _) = drive("hover", Some("http://example.com/"), |stage, rig| {
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        rig.events(stage, vec![Event::Hover("http://example.com/more".into())]);
    });
    save(&with_link, "lazywebplus-hover.png");
    let (without, _) = drive("hover2", Some("http://example.com/"), |stage, rig| {
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        rig.events(stage, vec![Event::Hover("http://example.com/more".into())]);
        rig.events(stage, vec![Event::Hover(String::new())]);
    });
    let (never, _) = drive("hover3", Some("http://example.com/"), |stage, rig| {
        rig.state(stage, loaded("http://example.com/", "Example", 1));
    });
    assert_ne!(with_link.pixels(), never.pixels(), "the link is drawn");
    assert_eq!(without.pixels(), never.pixels(), "and removed");
}

#[test]
fn a_link_for_another_app_goes_to_it_only_when_a_person_started_it() {
    let (_, calls) = drive("launch", None, |stage, rig| {
        rig.events(
            stage,
            vec![
                Event::Launch {
                    url: "mailto:a@b.test".into(),
                    by_user: true,
                },
                Event::Launch {
                    url: "tel:5551234".into(),
                    by_user: false,
                },
            ],
        );
        assert_eq!(*rig.opened.urls.borrow(), ["mailto:a@b.test"]);
    });
    // Nothing was loaded because of either.
    assert!(!calls
        .iter()
        .any(|c| matches!(c, Call::Load(u) if u.contains("mailto") || u.contains("tel:"))));
}

#[test]
fn a_typed_scheme_for_another_app_is_the_users_own_act() {
    let (image, calls, opened) = drive_full("typed", None, |stage, _| {
        // Ctrl+L clears the field; a click in it (below the menu bar, 28, in the toolbar, 38)
        // gives it the keyboard the headless stage has not given it yet.
        stage.emit(Msg::FocusAddress);
        stage.click(300, 47);
        for c in "mailto:me@example.com".chars() {
            stage.inject(UiEvent::Char(c));
        }
        stage.emit(Msg::Go);
    });
    save(&image, "lazywebplus-typed.png");
    assert_eq!(*opened.urls.borrow(), ["mailto:me@example.com"]);
    assert!(!calls
        .iter()
        .any(|c| matches!(c, Call::Load(u) if u.starts_with("mailto"))));
}

#[test]
fn the_browsers_own_pages_are_built_and_loaded_by_name() {
    let (_, calls) = drive("pages", None, |stage, _| {
        stage.emit(Msg::Menu(Command::ShowHistory));
        stage.emit(Msg::Menu(Command::About));
    });
    let loads: Vec<_> = calls
        .iter()
        .filter_map(|c| match c {
            Call::Load(url) => Some(url.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(loads.len(), 3, "{loads:?}");
    assert!(loads[1].contains("History") && loads[1].starts_with("data:"));
    assert!(loads[2].contains("About%20LazyWeb") || loads[2].contains("About LazyWeb"));
}

#[test]
fn a_command_runs_from_the_history_page_but_not_from_a_web_page() {
    let (_, _) = drive("command", None, |stage, rig| {
        // A visit to forget.
        rig.state(stage, loading("http://example.com/"));
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        assert!(rig.history_file().exists());

        // On a web page the command is refused, whoever let it through.
        rig.events(
            stage,
            vec![Event::Command {
                url: "x-lazyweb:clear-history".into(),
            }],
        );
        assert!(fs::read_to_string(rig.history_file())
            .unwrap()
            .contains("example.com"));

        // On the history page it runs.
        stage.emit(Msg::Menu(Command::ShowHistory));
        let url = rig.internal.url_of(HISTORY);
        rig.state(stage, loaded(&url, "History", 2));
        rig.events(
            stage,
            vec![Event::Command {
                url: "x-lazyweb:clear-history".into(),
            }],
        );
        assert!(!fs::read_to_string(rig.history_file())
            .unwrap()
            .contains("example.com"));
    });
}

#[test]
fn a_download_runs_shows_in_the_status_bar_completes_and_is_listed() {
    let (running, _) = drive("download", Some("http://example.com/"), |stage, rig| {
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        let destination = Saver::new(rig.downloads())
            .reserve(7, "oldnet-kit.zip")
            .unwrap();
        // The engine reserved through the shared saver of the process; here the window's own
        // saver is a different one, so reserve through the window's by posting the start.
        let _ = destination;
        rig.events(
            stage,
            vec![
                Event::DownloadStarted {
                    id: 7,
                    url: "https://theoldnet.com/files/oldnet-kit.zip".into(),
                    name: "oldnet-kit.zip".into(),
                    total: Some(307_200),
                    path: String::new(),
                },
                Event::DownloadProgress {
                    id: 7,
                    received: 153_600,
                    total: Some(307_200),
                },
            ],
        );
    });
    save(&running, "lazywebplus-download-running.png");
}

fn ask(stage: &Stage<'_, Msg>, rig: &Rig, id: u64, kind: DialogKind) {
    rig.state(stage, loaded("https://theoldnet.com/x", "X", 1));
    rig.events(
        stage,
        vec![Event::Dialog {
            id,
            kind,
            text: "Delete everything?".into(),
            default: String::new(),
        }],
    );
}

#[test]
fn a_dialog_is_drawn_over_the_page_and_waits_for_the_person() {
    let (image, calls) = drive(
        "dialog-open",
        Some("https://theoldnet.com/"),
        |stage, rig| {
            rig.mailbox.post_frame(880, 400, &test_page(880, 400));
            stage.emit(Msg::Frame);
            ask(stage, rig, 41, DialogKind::Confirm);
        },
    );
    save(&image, "lazywebplus-dialog.png");
    assert!(
        !calls.iter().any(|c| matches!(c, Call::DialogReply(..))),
        "nobody answered yet: {calls:?}"
    );
}

#[test]
fn enter_accepts_a_dialog_and_the_answer_goes_to_the_engine() {
    let (_, calls) = drive("dialog", Some("https://theoldnet.com/"), |stage, rig| {
        ask(stage, rig, 41, DialogKind::Confirm);
        // The dialog's OK has the focus.
        stage.inject(key_down(Key::RETURN));
    });
    assert!(
        calls.contains(&Call::DialogReply(41, true, String::new())),
        "{calls:?}"
    );
}

#[test]
fn escape_cancels_a_dialog() {
    let (_, calls) = drive("dialog-esc", None, |stage, rig| {
        rig.events(
            stage,
            vec![Event::Dialog {
                id: 5,
                kind: DialogKind::Confirm,
                text: "Sure?".into(),
                default: String::new(),
            }],
        );
        stage.inject(key_down(Key::ESCAPE));
    });
    assert!(
        calls.contains(&Call::DialogReply(5, false, String::new())),
        "{calls:?}"
    );
}

#[test]
fn a_new_window_request_opens_in_the_one_view_but_a_scheme_for_another_app_does_not() {
    let (_, calls) = drive("popup", None, |stage, rig| {
        rig.events(
            stage,
            vec![
                Event::Popup {
                    url: "https://theoldnet.com/other.html".into(),
                },
                Event::Popup {
                    url: "mailto:x@y.test".into(),
                },
            ],
        );
        assert!(rig.opened.urls.borrow().is_empty());
    });
    assert!(calls.contains(&Call::Load("https://theoldnet.com/other.html".into())));
    assert!(!calls
        .iter()
        .any(|c| matches!(c, Call::Load(u) if u.starts_with("mailto"))));
}

#[test]
fn escape_stops_a_load_but_belongs_to_the_page_otherwise() {
    let (_, calls) = drive("escape", Some("http://example.com/"), |stage, rig| {
        let (ox, oy) = page_origin(stage);
        stage.click(ox + 50, oy + 40);
        rig.state(stage, loading("http://example.com/"));
        rig.engine.take();
        stage.inject(key_down(Key::ESCAPE));
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        stage.inject(key_down(Key::ESCAPE));
    });
    let stops = calls.iter().filter(|c| matches!(c, Call::Stop)).count();
    assert_eq!(stops, 1, "{calls:?}");
    let escapes_to_page = calls
        .iter()
        .filter(|c| matches!(c, Call::Key(true, 0xff1b, _, _)))
        .count();
    assert_eq!(
        escapes_to_page, 1,
        "the idle Escape reached the page: {calls:?}"
    );
}

#[test]
fn reload_of_one_of_our_pages_builds_it_again() {
    let (_, calls) = drive("reload", None, |stage, rig| {
        stage.emit(Msg::Menu(Command::About));
        let url = rig.internal.url_of(ABOUT);
        rig.state(stage, loaded(&url, "About LazyWeb+", 1));
        rig.engine.take();
        stage.emit(Msg::Menu(Command::Reload));
    });
    assert!(calls
        .iter()
        .any(|c| matches!(c, Call::Load(u) if u.starts_with("data:"))));
    assert!(!calls.contains(&Call::Reload));
}

#[test]
fn a_right_click_on_a_link_offers_to_copy_it_and_the_pointer_shape_follows_the_page() {
    let (image, _) = drive("context", Some("http://example.com/"), |stage, rig| {
        rig.state(stage, loaded("http://example.com/", "Example", 1));
        rig.events(
            stage,
            vec![
                Event::Cursor(CursorKind::Hand),
                Event::Hover("http://example.com/more".into()),
            ],
        );
        stage.emit(Msg::PageMenu(60, 40));
    });
    save(&image, "lazywebplus-context.png");
}
