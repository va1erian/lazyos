//! The window's behavior behind an administrator's prompt: the store's
//! `sys/**` writes wait at a gate, as they do behind a password prompt, so
//! a test can tell a window that keeps working from one that stands still.
//!
//! Split from `window.rs` for the 500-line limit (issue #484).

use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use xui_canvas::snapshot::{render_with, Snapshot};
use xui_core::Dip;
use xui_settings::app::WINDOW;
use xui_settings::menu_page::MenuMsg;
use xui_settings::store::{AppChoice, ConfigStore, Value};
use xui_settings::{MemAccounts, MemSystem, Msg, Section, SettingsApp};

/// The fonts the window paints its text with (xui loads its own family on
/// use; these set the canvas default the widgets see).
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

/// Run `test` on its own thread with the 120 s limit: the window is one
/// thread's whole world, and a worker stuck inside it would hang the suite.
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
        // A worker still running past the deadline has hung: joining it would
        // hang the run too, so fail now.
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("the window test hung"),
        Err(mpsc::RecvTimeoutError::Disconnected) => match handle.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => panic!("the window test ended without a result"),
        },
    }
}
/// One store several threads share. A machine write (`sys/**`) waits at a
/// gate, as it does behind an administrator's password prompt, so a test can
/// tell a window that keeps running from one that stands still.
struct Gated {
    map: std::sync::Mutex<std::collections::BTreeMap<String, Value>>,
    open: (std::sync::Mutex<bool>, std::sync::Condvar),
    entered: std::sync::atomic::AtomicBool,
    /// The app registry the pages see (the Menu page pins from it).
    apps: Vec<AppChoice>,
    /// The user the pages act as.
    uid: u32,
}
struct GatedStore(std::sync::Arc<Gated>);

impl ConfigStore for GatedStore {
    fn get(&self, key: &str) -> Option<Value> {
        self.0.map.lock().unwrap().get(key).cloned()
    }

    fn set(&self, key: &str, value: Value) -> Result<(), String> {
        if key.starts_with("sys/") {
            self.0
                .entered
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let (lock, wake) = &self.0.open;
            let guard = lock.lock().unwrap();
            // Bounded, so a window that wrongly runs this on its own thread
            // fails the timing check instead of hanging the suite.
            let _ = wake
                .wait_timeout_while(guard, Duration::from_secs(10), |open| !*open)
                .unwrap();
        }
        self.0.map.lock().unwrap().insert(key.to_owned(), value);
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        self.0.map.lock().unwrap().remove(key);
        Ok(())
    }

    fn uid(&self) -> Option<u32> {
        Some(1000)
    }

    fn apps(&self) -> Vec<AppChoice> {
        self.0.apps.clone()
    }

    fn detached(&self) -> Option<xui_settings::store::Detached> {
        let shared = std::sync::Arc::clone(&self.0);
        Some(std::sync::Arc::new(move || {
            Box::new(GatedStore(std::sync::Arc::clone(&shared))) as Box<dyn ConfigStore>
        }))
    }
}

/// "Make this the default for everyone" waits on the administrator's prompt
/// on a worker thread: the window answers at once, and the result lands when
/// the prompt is answered.
#[test]
fn making_the_theme_the_default_does_not_stand_the_window_still() {
    use std::sync::atomic::Ordering;
    use std::time::Instant;
    use xui_core::backend::Event;

    watchdog(|| {
        let own = uitheme::user_key(1000, uitheme::KEY_ACCENT).expect("a personal key");
        let shared = std::sync::Arc::new(Gated {
            map: Default::default(),
            open: Default::default(),
            entered: Default::default(),
            apps: Vec::new(),
            uid: 1000,
        });
        shared
            .map
            .lock()
            .unwrap()
            .insert(own.clone(), Value::U64(0xAA3232));
        let build = std::sync::Arc::clone(&shared);
        let probe = std::sync::Arc::clone(&shared);
        render_with(
            Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)),
            move |ui| {
                SettingsApp::build(
                    ui,
                    Rc::new(GatedStore(build)),
                    Rc::new(MemSystem::default()),
                    Rc::new(MemAccounts::default()),
                )
            },
            move |stage| {
                let started = Instant::now();
                stage.emit(Msg::MakeDefault);
                assert!(
                    started.elapsed() < Duration::from_secs(5),
                    "the message waited for the prompt"
                );
                // A second press while the first waits is refused, not queued.
                stage.emit(Msg::MakeDefault);
                let deadline = Instant::now() + Duration::from_secs(5);
                while !probe.entered.load(Ordering::SeqCst) {
                    assert!(Instant::now() < deadline, "the worker never asked");
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert_eq!(probe.map.lock().unwrap().get(uitheme::KEY_ACCENT), None);
                // The administrator approves.
                *probe.open.0.lock().unwrap() = true;
                probe.open.1.notify_all();
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    stage.inject(Event::Wake);
                    let map = probe.map.lock().unwrap();
                    if map.get(uitheme::KEY_ACCENT) == Some(&Value::U64(0xAA3232))
                        && !map.contains_key(&own)
                    {
                        break;
                    }
                    drop(map);
                    assert!(Instant::now() < deadline, "the write never finished");
                    std::thread::sleep(Duration::from_millis(5));
                }
                // The result is handled on this thread without a panic.
                stage.inject(Event::Wake);
            },
        )
        .expect("the headless render");
    });
}

/// The Menu page's Save waits on an administrator on the worker thread
/// while the page keeps working: an edit typed while the prompt is up is
/// still in the draft when the save lands, and a second Save writes those
/// edits too (`saved_in_background`, issue #720's follow-up).
#[test]
fn menu_draft_survives_the_worker_save() {
    use std::sync::atomic::Ordering;
    use xui_core::backend::Event;

    watchdog(|| {
        let shared = std::sync::Arc::new(Gated {
            map: Default::default(),
            open: Default::default(),
            entered: Default::default(),
            apps: ["Files", "Paint", "Terminal"]
                .iter()
                .map(|name| AppChoice {
                    id: format!("os.lazy.{}", name.to_lowercase()),
                    name: (*name).to_owned(),
                    desktop: true,
                })
                .collect(),
            uid: 1000,
        });
        let build = std::sync::Arc::clone(&shared);
        let probe = std::sync::Arc::clone(&shared);
        render_with(
            Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)),
            move |ui| {
                let store: Rc<dyn ConfigStore> = Rc::new(GatedStore(build));
                SettingsApp::build(
                    ui,
                    store,
                    Rc::new(MemSystem::default()),
                    Rc::new(MemAccounts::default()),
                )
            },
            move |stage| {
                let menu = Msg::Menu;
                stage.emit(Msg::Section(Section::Menu.index()));
                // Two adds make the draft unsaved; Save takes the worker.
                stage.emit(menu(MenuMsg::Add));
                stage.emit(menu(MenuMsg::Add));
                stage.emit(menu(MenuMsg::Save));
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while !probe.entered.load(Ordering::SeqCst) {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the worker never asked"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                // An edit typed behind the prompt joins the draft.
                stage.emit(menu(MenuMsg::Add));
                // An id conveyed by the third Add (one past the two saved).
                const THIRD: &str = "os.lazy.terminal";
                // The administrator approves: the first save lands with the
                // two apps it captured and the store keeps what was saved.
                *probe.open.0.lock().unwrap() = true;
                probe.open.1.notify_all();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    stage.inject(Event::Wake);
                    let entry = probe.map.lock().unwrap().get(deskmenu::KEY).cloned();
                    let Some(entry) = entry else {
                        assert!(
                            std::time::Instant::now() < deadline,
                            "the save never finished"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    };
                    let saved = deskmenu::from_value(Some(&entry), &|_| true);
                    let ids: Vec<&str> = saved.iter().map(|item| item.app.as_str()).collect();
                    assert_eq!(ids, ["os.lazy.files", "os.lazy.paint"]);
                    break;
                }
                // A second Save writes the whole surviving draft.
                stage.emit(menu(MenuMsg::Save));
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    stage.inject(Event::Wake);
                    let entry = probe.map.lock().unwrap().get(deskmenu::KEY).cloned();
                    if let Some(entry) = entry {
                        let list = deskmenu::from_value(Some(&entry), &|_| true);
                        if list.len() >= 3 {
                            assert_eq!(list[2].app, THIRD);
                            break;
                        }
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "the second save never wrote the surviving draft"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            },
        )
        .expect("the headless render");
    });
}
