//! The window built offscreen in both modes: the account page a first start
//! shows, and the mail view behind it. Snapshots and the layout report go to
//! `target/snapshots/mail-*` for a human to look at.

use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use esmail::config::Config;
use xui_canvas::snapshot::{Snapshot, render_with};
use xui_core::{Dip, Image, Theme};

use super::{Mail, Msg};

const WINDOW: (f32, f32) = (1000.0, 620.0);

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

/// Runs `test` on its own thread with the fonts, failing if it hangs.
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

/// The window with no account, after `msgs`, and its layout report.
fn render(dark: bool, msgs: Vec<Msg>) -> (Image, String) {
    let theme = if dark { Theme::dark() } else { Theme::light() };
    let report = std::rc::Rc::new(std::cell::RefCell::new(String::new()));
    let out = std::rc::Rc::clone(&report);
    let image = render_with(
        Snapshot::new(Dip(WINDOW.0), Dip(WINDOW.1)).theme(theme),
        |ui| Mail::build(ui, Config::default()),
        move |stage| {
            for msg in msgs {
                stage.emit(msg);
            }
            *out.borrow_mut() = stage.ui().layout_report();
        },
    )
    .expect("the headless render");
    let report = report.borrow().clone();
    (image, report)
}

fn save(name: &str, image: &Image, report: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(format!("{name}.png"))).unwrap();
    std::fs::write(dir.join(format!("{name}.txt")), report).unwrap();
}

#[test]
fn account_page_and_mail_view_lay_out_in_both_modes() {
    watchdog(|| {
        for dark in [false, true] {
            let mode = if dark { "dark" } else { "light" };
            let (image, report) = render(dark, Vec::new());
            save(&format!("mail-account-{mode}"), &image, &report);
            assert!(!report.contains("overlap"), "{report}");
            let (image, report) = render(dark, vec![Msg::CloseAccount]);
            save(&format!("mail-view-{mode}"), &image, &report);
            assert!(!report.contains("overlap"), "{report}");
        }
    });
}
