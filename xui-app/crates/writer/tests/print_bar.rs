//! The print bar in a live window: Ctrl+P shows it, Print hands the document
//! to the print spooler as the timer ticks, the spooler sends it to a fake
//! IPP printer, and the status ends in the printer's verdict and ink levels.
//! The window is saved with the bar shown for a human to look at
//! (`target/snapshots/writer_print_bar.png`). Quitting while the document is
//! still being prepared asks first, and Stop printing sends the printer
//! nothing.

mod common;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use printd::fake::{Behaviour, FakePrinter};
use xui_core::Theme;
use xui_core::backend::Event;
use xui_core::message::{Key, Modifiers};
use xui_core::widget::{HasText, TaskDialogAction};
use xui_rich_text::edit::Command;
use xui_writer::Msg;

use common::{TempDir, pump, render, save, watchdog};

fn ctrl_p() -> Event {
    Event::KeyDown {
        key: Key::P,
        modifiers: Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        },
        repeat: 1,
        system: false,
    }
}

#[test]
fn ctrl_p_shows_the_bar_and_print_sends_the_document() {
    let printer = FakePrinter::start(Behaviour::default());
    let address = printer.address();

    let (status, image) = watchdog(move || {
        let dir = TempDir::new("print");
        let out: Rc<RefCell<String>> = Rc::default();
        let keep = Rc::clone(&out);
        let image = render(Theme::light(), dir.0.clone(), move |stage, rig| {
            rig.editor
                .exec(Command::InsertText("A letter to the DeskJet".into()));
            pump(stage);
            stage.inject(ctrl_p());
            rig.print_printer.set_text(&address);
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
    save(&image, "writer_print_bar.png");
    assert_eq!(status, "Printed (ink: tri-color 90%, black 50%)");
    let seen = printer.seen();
    assert_eq!(seen.truncated, 0);
    assert!(seen.open_jobs().is_empty());
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

#[test]
fn quitting_while_preparing_asks_and_stop_sends_nothing() {
    let printer = FakePrinter::start(Behaviour::default());
    let address = printer.address();
    let (asked, status) = watchdog(move || {
        let dir = TempDir::new("print-quit");
        let out: Rc<RefCell<(bool, String)>> = Rc::default();
        let keep = Rc::clone(&out);
        render(Theme::light(), dir.0.clone(), move |stage, rig| {
            // Enough pages that the job is still being prepared below.
            let text = "A long letter to the DeskJet\n".repeat(200);
            rig.editor.exec(Command::InsertText(text));
            pump(stage);
            stage.inject(ctrl_p());
            rig.print_printer.set_text(&address);
            stage.emit(Msg::PrintStart);
            stage.emit(Msg::PrintTick);
            stage.emit(Msg::Quit);
            let asked = rig.dialog_open.get();
            stage.emit(Msg::StopPrinting(TaskDialogAction::Command(0)));
            *keep.borrow_mut() = (asked, rig.print_status.text());
        });
        out.take()
    });
    assert!(asked, "a quit mid-print asks first");
    assert_eq!(status, "Printing canceled");
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        printer.seen().operations.is_empty(),
        "the printer got nothing"
    );
}
