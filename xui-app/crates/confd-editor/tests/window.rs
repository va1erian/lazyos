//! The Config window built offscreen: a key selected, then the create-key
//! form open, in both themes, with a snapshot of each
//! (`target/snapshots/confd-*.png`) for a human to look at.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use confd::Value;
use xui_canvas::snapshot::{render_with, Snapshot};
use xui_confd_editor::WINDOW;
use xui_confd_editor::{ConfStore, ConfdEditorApp, MemStore, Msg, Scope, StoreError, StoreInfo};
use xui_core::{Dip, Image, Theme};

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
        // A worker still running past the deadline has hung: joining it would
        // hang the run too, so fail now.
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("the window test hung"),
        Err(mpsc::RecvTimeoutError::Disconnected) => match handle.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => panic!("the window test ended without a result"),
        },
    }
}

/// The window in `theme` after `messages`, over a few seeded keys.
fn render(theme: Theme, messages: Vec<Msg>) -> Image {
    render_with(
        Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)).theme(theme),
        |ui| {
            let store = MemStore::new();
            store.seed("sys/ui/mode", Value::Str("dark".into()));
            store.seed("sys/ui/anim", Value::Bool(true));
            store.seed("sys/time/hour24", Value::Bool(false));
            ConfdEditorApp::build(ui, Rc::new(store), Scope::everything())
        },
        move |stage| {
            for msg in messages {
                stage.emit(msg);
            }
        },
    )
    .expect("the headless render")
}

fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

#[test]
fn a_selected_key_and_the_create_form_render_in_both_themes() {
    watchdog(|| {
        for (theme, tag) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
            // Open `sys`, then `sys/time`, then select its one key.
            let select = vec![Msg::Select(0), Msg::Select(1), Msg::Select(2)];
            save(&render(theme, select), &format!("confd-selected-{tag}.png"));
            let create = vec![Msg::NewToggle, Msg::NewPath("sys/ui/demo".into())];
            save(&render(theme, create), &format!("confd-new-key-{tag}.png"));
        }
    });
}

/// A store that records what the window asked of it (the prefixes it listed,
/// with what came back, and the paths it read), over a shared [`MemStore`].
struct Recording {
    inner: Rc<MemStore>,
    lists: RefCell<Vec<(String, Vec<String>)>>,
    reads: RefCell<Vec<String>>,
}

impl Recording {
    fn over(inner: &Rc<MemStore>) -> Rc<Recording> {
        Rc::new(Recording {
            inner: inner.clone(),
            lists: RefCell::new(Vec::new()),
            reads: RefCell::new(Vec::new()),
        })
    }

    /// Every prefix listed, in order.
    fn prefixes(&self) -> Vec<String> {
        self.lists.borrow().iter().map(|(p, _)| p.clone()).collect()
    }

    /// The paths the last listing returned.
    fn listed(&self) -> Vec<String> {
        self.lists
            .borrow()
            .last()
            .map(|(_, paths)| paths.clone())
            .unwrap_or_default()
    }
}

impl ConfStore for Recording {
    fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        let paths = self.inner.list(prefix)?;
        self.lists
            .borrow_mut()
            .push((prefix.to_owned(), paths.clone()));
        Ok(paths)
    }
    fn get(&self, path: &str) -> Result<Option<Value>, StoreError> {
        self.reads.borrow_mut().push(path.to_owned());
        self.inner.get(path)
    }
    fn set(&self, path: &str, value: Value) -> Result<(), StoreError> {
        self.inner.set(path, value)
    }
    fn delete(&self, path: &str) -> Result<(), StoreError> {
        self.inner.delete(path)
    }
    fn info(&self) -> Result<StoreInfo, StoreError> {
        self.inner.info()
    }
}

/// An elevation that grants `elevated` (a test double of `elevd` after an
/// administrator approved), or refuses as a cancelled prompt does.
struct Granting(Option<Rc<Recording>>);

impl xui_confd_editor::Elevation for Granting {
    fn elevate(&self) -> Result<Rc<dyn ConfStore>, String> {
        match &self.0 {
            Some(store) => Ok(store.clone() as Rc<dyn ConfStore>),
            None => Err(String::from("cancelled")),
        }
    }
}

/// The window in user 1000's own scope over `own`, elevating to `elevated`
/// (refused when `None`), after `messages`.
fn render_own(own: Rc<Recording>, elevated: Option<Rc<Recording>>, messages: Vec<Msg>) -> Image {
    render_with(
        Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)).theme(Theme::dark()),
        move |ui| {
            let scope = Scope::own(1000, Rc::new(Granting(elevated)));
            ConfdEditorApp::build(ui, own, scope)
        },
        move |stage| {
            for msg in messages {
                stage.emit(msg);
            }
        },
    )
    .expect("the headless render")
}

#[test]
fn the_own_scope_then_elevated_render() {
    watchdog(|| {
        let store = Rc::new(MemStore::new());
        store.seed("sys/ui/mode", Value::Str("dark".into()));
        store.seed("user/1000/ui/accent", Value::U64(7));
        store.seed("user/1001/ui/accent", Value::U64(9));

        // Own scope: only `user/1000` is listed and read, never a machine key
        // nor another user's.
        let own = Recording::over(&store);
        save(&render_own(own.clone(), None, Vec::new()), "confd-own.png");
        assert!(!own.prefixes().is_empty(), "the tree was never listed");
        assert!(
            own.prefixes().iter().all(|p| p == "user/1000"),
            "listed outside the own scope: {:?}",
            own.prefixes()
        );
        assert_eq!(own.listed(), ["user/1000/ui/accent"]);
        assert!(
            own.reads
                .borrow()
                .iter()
                .all(|p| p.starts_with("user/1000/")),
            "read outside the own scope: {:?}",
            own.reads.borrow()
        );

        // A refused elevation keeps the own scope.
        let own = Recording::over(&store);
        render_own(own.clone(), None, vec![Msg::Elevate]);
        assert!(own.prefixes().iter().all(|p| p == "user/1000"));

        // Elevated: every key, listed through the elevated store only.
        let own = Recording::over(&store);
        let elevated = Recording::over(&store);
        let image = render_own(own.clone(), Some(elevated.clone()), vec![Msg::Elevate]);
        save(&image, "confd-elevated.png");
        assert_eq!(elevated.prefixes().last().map(String::as_str), Some(""));
        assert_eq!(
            elevated.listed(),
            ["sys/ui/mode", "user/1000/ui/accent", "user/1001/ui/accent"]
        );
        assert!(
            own.prefixes().iter().all(|p| p == "user/1000"),
            "the user's own store listed past its scope: {:?}",
            own.prefixes()
        );
    });
}
