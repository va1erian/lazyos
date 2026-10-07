//! The Settings window built offscreen: every section shows on its own, in
//! both modes, and a snapshot of each (`target/snapshots/settings-*.png`) is
//! left for a human to look at.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use uitheme::Mode;
use xui_canvas::snapshot::{render_with, Snapshot};
use xui_core::{Dip, Image};
use xui_settings::app::WINDOW;
use xui_settings::store::AppChoice;
use xui_settings::{theme_ops, MemStore, MemSystem, Msg, Section, SettingsApp};

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

/// A store in `mode` with a few registry apps for the Menu and Hidden pages.
fn store(mode: Mode) -> MemStore {
    let store = MemStore::new();
    theme_ops::set_mode(&store, mode).unwrap();
    *store.uid.borrow_mut() = Some(1000);
    *store.apps.borrow_mut() = ["Files", "Paint", "Terminal"]
        .iter()
        .map(|name| AppChoice {
            id: format!("os.lazy.{}", name.to_lowercase()),
            name: (*name).to_owned(),
            desktop: true,
        })
        .collect();
    store
}

/// The window in `mode` after a switch to `section`.
fn render(mode: Mode, section: Section) -> Image {
    render_with(
        Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)),
        move |ui| {
            SettingsApp::build(
                ui,
                Rc::new(store(mode)),
                Rc::new(MemSystem::default()),
                Rc::new(xui_settings::MemAccounts::default()),
            )
        },
        move |stage| stage.emit(Msg::Section(section.index())),
    )
    .expect("the headless render")
}

fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

#[test]
fn every_section_renders_in_both_modes() {
    watchdog(|| {
        for (mode, tag) in [(Mode::Light, "light"), (Mode::Dark, "dark")] {
            for section in Section::ALL {
                let image = render(mode, section);
                let name = section.label().to_lowercase().replace([' ', '&'], "");
                save(&image, &format!("settings-{name}-{tag}.png"));
            }
        }
    });
}
