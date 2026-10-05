//! The print bar in a live window: Ctrl+P shows it, Print sends the
//! document to a fake IPP printer as the timer ticks, and the status ends in
//! the printer's verdict and ink levels. The window is saved with the bar
//! shown for a human to look at (`target/snapshots/writer_print_bar.png`).

mod common;

use std::cell::RefCell;
use std::net::TcpListener;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use xui_core::Theme;
use xui_core::backend::Event;
use xui_core::message::{Key, Modifiers};
use xui_core::widget::HasText;
use xui_rich_text::edit::Command;
use xui_writer::Msg;

use common::printer::{Seen, fake_printer};
use common::{TempDir, pump, render, save, watchdog};

#[test]
fn ctrl_p_shows_the_bar_and_print_sends_the_document() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let server = {
        let seen = Arc::clone(&seen);
        std::thread::spawn(move || fake_printer(listener, seen))
    };

    let (status, image) = watchdog(move || {
        let dir = TempDir::new("print");
        let out: Rc<RefCell<String>> = Rc::default();
        let keep = Rc::clone(&out);
        let image = render(Theme::light(), dir.0.clone(), move |stage, rig| {
            rig.editor
                .exec(Command::InsertText("A letter to the DeskJet".into()));
            pump(stage);
            stage.inject(Event::KeyDown {
                key: Key::P,
                modifiers: Modifiers {
                    ctrl: true,
                    ..Modifiers::NONE
                },
                repeat: 1,
                system: false,
            });
            rig.print_printer.set_text(&format!("127.0.0.1:{port}"));
            stage.emit(Msg::PrintStart);
            let start = Instant::now();
            loop {
                stage.emit(Msg::PrintTick);
                let status = rig.print_status.text();
                if status.starts_with("Printed") || status.starts_with("Could not") {
                    *keep.borrow_mut() = status;
                    break;
                }
                assert!(
                    start.elapsed() < Duration::from_secs(90),
                    "stuck at {status:?}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let status = out.take();
        (status, image)
    });
    server.join().unwrap();
    save(&image, "writer_print_bar.png");
    assert_eq!(status, "Printed (ink: tri-color 90%, black 50%)");
    let seen = seen.lock().unwrap();
    let pages = raster::decode(&seen.document, 1 << 30).unwrap();
    assert_eq!(pages.len(), 1);
    assert_eq!(
        (
            pages[0].header.width,
            pages[0].header.height,
            pages[0].header.dpi
        ),
        (2480, 3508, 300),
        "A4 at 300 dpi"
    );
    assert!(
        pages[0].pixels.iter().any(|&p| p < 128),
        "the text is on the page"
    );
}
