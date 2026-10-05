//! The Calculator window built offscreen: it renders in both themes (left in
//! `target/snapshots/calc-*.png` for a human to look at), and clicks at the
//! keypad positions the QEMU session uses press the keys they should.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use xui_calc::{CalcApp, Msg, Op, Press, Report, WINDOW};
use xui_canvas::snapshot::{render_with, Snapshot, Stage};
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
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("the window test hung"),
        Err(mpsc::RecvTimeoutError::Disconnected) => match handle.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => panic!("the window test ended without a result"),
        },
    }
}

/// The window in `theme` after `drive`, with every report it made.
fn render(theme: Theme, drive: impl FnOnce(&Stage<'_, Msg>) + 'static) -> (Image, Vec<String>) {
    let reports = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&reports);
    let image = render_with(
        Snapshot::new(Dip(WINDOW.0 as f32), Dip(WINDOW.1 as f32)).theme(theme),
        move |ui| {
            CalcApp::build(ui, move |report| {
                log.borrow_mut().push(match report {
                    Report::Msg(msg) => format!("MSG:{msg:?}"),
                    Report::Result(shown) => format!("RESULT:{shown}"),
                })
            })
        },
        drive,
    )
    .expect("the headless render");
    let reports = reports.borrow().clone();
    (image, reports)
}

fn save(image: &Image, name: &str) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    image.save_png(dir.join(name)).unwrap();
}

/// The centre of keypad cell (`column`, `row`) in window pixels at 96 DPI:
/// 12 px padding, the 84 px display and a 12 px gap above a grid of 4x5
/// equal cells 6 px apart. `tools/screenshot/examples/xui_calc.json` clicks
/// the same points (offset by the window's content origin).
fn key_center(column: i32, row: i32) -> (i32, i32) {
    let (width, height) = WINDOW;
    let (left, top) = (12.0, 12.0 + 84.0 + 12.0);
    let cell_w = (width as f32 - 24.0 - 3.0 * 6.0) / 4.0;
    let cell_h = (height as f32 - 12.0 - top - 4.0 * 6.0) / 5.0;
    (
        (left + column as f32 * (cell_w + 6.0) + cell_w / 2.0).round() as i32,
        (top + row as f32 * (cell_h + 6.0) + cell_h / 2.0).round() as i32,
    )
}

/// Every key the session presses: (label, column, row).
const SESSION_KEYS: [(&str, i32, i32); 7] = [
    ("1", 0, 3),
    ("2", 1, 3),
    ("+", 3, 3),
    ("7", 0, 1),
    ("x", 3, 1),
    ("3", 2, 3),
    ("=", 3, 4),
];

#[test]
fn renders_in_both_themes() {
    watchdog(|| {
        for (theme, tag) in [(Theme::light(), "light"), (Theme::dark(), "dark")] {
            let (image, _) = render(theme, |_| {});
            save(&image, &format!("calc-initial-{tag}.png"));
            let (image, _) = render(theme, |stage| {
                for press in [
                    Press::Digit(1),
                    Press::Digit(2),
                    Press::Op(Op::Add),
                    Press::Digit(7),
                    Press::Op(Op::Mul),
                    Press::Digit(3),
                ] {
                    stage.emit(Msg::Press(press));
                }
            });
            save(&image, &format!("calc-typing-{tag}.png"));
        }
        let (image, _) = render(Theme::light(), |stage| {
            for press in [Press::Digit(5), Press::Op(Op::Div), Press::Digit(0)] {
                stage.emit(Msg::Press(press));
            }
            stage.emit(Msg::Press(Press::Equals));
        });
        save(&image, "calc-error-light.png");
        // A 14-character result shrinks to fit the display.
        let (image, _) = render(Theme::dark(), |stage| {
            for press in [Press::Digit(1), Press::Op(Op::Div), Press::Digit(7)] {
                stage.emit(Msg::Press(press));
            }
            stage.emit(Msg::Press(Press::Equals));
        });
        save(&image, "calc-long-dark.png");
    });
}

#[test]
fn session_clicks_compute_57() {
    let reports = watchdog(|| {
        let (image, reports) = render(Theme::light(), |stage| {
            for (_, column, row) in SESSION_KEYS {
                let (x, y) = key_center(column, row);
                stage.click(x, y);
            }
        });
        save(&image, "calc-result-light.png");
        reports
    });
    let pressed: Vec<&String> = reports.iter().filter(|r| r.starts_with("MSG:")).collect();
    assert_eq!(
        pressed,
        [
            "MSG:Press(Digit(1))",
            "MSG:Press(Digit(2))",
            "MSG:Press(Op(Add))",
            "MSG:Press(Digit(7))",
            "MSG:Press(Op(Mul))",
            "MSG:Press(Digit(3))",
            "MSG:Press(Equals)",
        ],
        "{reports:?}"
    );
    assert_eq!(reports.last().map(String::as_str), Some("RESULT:57"));
}

#[test]
fn every_key_is_where_the_grid_says() {
    let labels = [
        "Clear",
        "Backspace",
        "Percent",
        "Op(Div)",
        "Digit(7)",
        "Digit(8)",
        "Digit(9)",
        "Op(Mul)",
        "Digit(4)",
        "Digit(5)",
        "Digit(6)",
        "Op(Sub)",
        "Digit(1)",
        "Digit(2)",
        "Digit(3)",
        "Op(Add)",
        "Negate",
        "Digit(0)",
        "Point",
        "Equals",
    ];
    let reports = watchdog(|| {
        let (_, reports) = render(Theme::light(), |stage| {
            for row in 0..5 {
                for column in 0..4 {
                    let (x, y) = key_center(column, row);
                    stage.click(x, y);
                }
            }
        });
        reports
    });
    let pressed: Vec<String> = reports
        .into_iter()
        .filter_map(|r| {
            r.strip_prefix("MSG:Press(")
                .map(|p| p.trim_end_matches(')').to_owned())
        })
        .collect();
    let want: Vec<String> = labels
        .iter()
        .map(|l| l.trim_end_matches(')').to_owned())
        .collect();
    assert_eq!(pressed, want);
}
