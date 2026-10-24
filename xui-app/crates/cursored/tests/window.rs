//! The Cursor Studio window built offscreen: a snapshot of the editor on a
//! built-in shape and on a customised one (`target/snapshots/cursored-*.png`,
//! left for a human to look at), and Apply writing the user's key.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use confd::Value;
use xui_canvas::snapshot::{render_with, Snapshot};
use xui_core::{Dip, Image};
use xui_cursored::app::{Studio, WINDOW};
use xui_cursored::store::ThemeStore;
use xui_cursored::Msg;

#[derive(Default)]
struct Mem {
    map: RefCell<HashMap<String, Value>>,
}

impl ThemeStore for Mem {
    fn uid(&self) -> Option<u32> {
        Some(1000)
    }
    fn get(&self, key: &str) -> Option<Value> {
        self.map.borrow().get(key).cloned()
    }
    fn set(&self, key: &str, value: Value) -> Result<(), String> {
        self.map.borrow_mut().insert(key.into(), value);
        Ok(())
    }
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
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("the window test hung"),
        Err(mpsc::RecvTimeoutError::Disconnected) => match handle.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => panic!("the window test ended without a result"),
        },
    }
}

fn render(store: Rc<Mem>, messages: Vec<Msg>) -> Image {
    render_with(
        Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)),
        move |ui| Studio::build(ui, store),
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
fn the_editor_renders_a_builtin_and_a_customised_shape() {
    watchdog(|| {
        let store = Rc::new(Mem::default());
        save(&render(store.clone(), vec![]), "cursored-builtin.png");
        let custom = vec![Msg::Pick(3), Msg::Customize, Msg::Fill("2060e0".into())];
        save(&render(store, custom), "cursored-custom.png");
    });
}

#[test]
fn apply_writes_the_users_key_and_a_bad_name_is_refused() {
    watchdog(|| {
        let store = Rc::new(Mem::default());
        render(store.clone(), vec![Msg::Name("mine".into()), Msg::Apply]);
        let text = match store.get("user/1000/ui/cursor/theme") {
            Some(Value::Str(text)) => text,
            other => panic!("no theme written: {other:?}"),
        };
        assert!(text.starts_with("lazyos-cursor 1\nname mine\n"));

        let refused = Rc::new(Mem::default());
        render(
            refused.clone(),
            vec![Msg::Name("Bad Name".into()), Msg::Apply],
        );
        assert!(refused.map.borrow().is_empty());
    });
}

#[test]
fn editing_then_applying_round_trips_through_the_store() {
    watchdog(|| {
        let store = Rc::new(Mem::default());
        let edit = vec![
            Msg::Pick(3),
            Msg::Customize,
            Msg::Fill("2060e0".into()),
            Msg::Name("blue".into()),
            Msg::Apply,
        ];
        render(store.clone(), edit);
        let theme = xui_cursored::store::current(store.as_ref()).expect("a stored theme");
        assert_eq!(theme.name, "blue");
        let hand = theme.shapes[3].as_ref().expect("the hand is customised");
        assert_eq!(hand.layers[0].fill, 0x2060e0);
        // The window opens on what is applied.
        render(store.clone(), vec![]);
    });
}
