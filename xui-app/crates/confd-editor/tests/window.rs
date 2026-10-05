//! The Config window built offscreen: a key selected, then the create-key
//! form open, in both themes, with a snapshot of each
//! (`target/snapshots/confd-*.png`) for a human to look at.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use confd::Value;
use xui_canvas::snapshot::{render_with, Snapshot};
use xui_confd_editor::{ConfdEditorApp, MemStore, Msg, WINDOW};
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
            ConfdEditorApp::build(ui, Rc::new(store))
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
