//! The picture shown ahead of the page, as pixels: a wheel notch moves what is on show at once with a
//! checkerboard where it uncovers nothing painted, and the page's answer (an update that carries the
//! offset it painted at) puts the exact picture back with no checkerboard left (the rules are in
//! `speculate.rs`, tested in `speculate.rs` under `tests/`).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use xui_canvas::snapshot::{render_with, Gallery, Snapshot, Stage};
use xui_core::backend::Event;
use xui_core::{Dip, Image, Modifiers, Theme};
use xui_webkit::app::{Browser, Host, Msg};
use xui_webkit::damage::{IRect, Scroll};
use xui_webkit::engine::{Call, Recorder};
use xui_webkit::mailbox::{Delta, Mailbox, Patch};

const SIZE: (i32, i32) = (800, 500);
const BLUE: [u8; 3] = [0, 0, 255];
const LIGHT: [u8; 3] = [243, 243, 243];
const DARK: [u8; 3] = [229, 229, 229];
/// Pixels one wheel notch moves the picture ahead (`speculate::LINE_PX`).
const STEP: i32 = 40;

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

/// The colour of page row `y`: different from its neighbours', so a row that moved is noticed.
fn row_colour(y: i32) -> [u8; 3] {
    [(y * 7 % 251) as u8, (y * 13 % 241) as u8, 100]
}

fn rows(width: i32, height: i32, first_row: i32) -> Vec<u8> {
    (0..height)
        .flat_map(|y| {
            let c = row_colour(first_row + y);
            (0..width).flat_map(move |_| [c[0], c[1], c[2], 255])
        })
        .collect()
}

fn solid(width: i32, height: i32, c: [u8; 3]) -> Vec<u8> {
    (0..width * height)
        .flat_map(|_| [c[0], c[1], c[2], 255])
        .collect()
}

struct Shown {
    image: Image,
    origin: (i32, i32),
    size: (i32, i32),
}

impl Shown {
    fn at(&self, x: i32, y: i32) -> [u8; 3] {
        let width = self.image.width() as i32;
        let at = (((self.origin.1 + y) * width + self.origin.0 + x) * 4) as usize;
        let p = &self.image.pixels()[at..at + 3];
        [p[0], p[1], p[2]]
    }
}

/// A frame of banded rows at scroll offset 0, shown; then `script` (wheel events, the page's answer).
fn run(script: impl FnOnce(&Stage<'_, Msg>, (i32, i32), &Mailbox, (i32, i32)) + 'static) -> Shown {
    run_with(false, script)
}

/// `run`, with smooth scrolling on or off (off: a notch lands at once, where these tests look for it).
fn run_with(
    smooth: bool,
    script: impl FnOnce(&Stage<'_, Msg>, (i32, i32), &Mailbox, (i32, i32)) + 'static,
) -> Shown {
    xui_webkit::glide::set_smooth(smooth);
    register_fonts();
    let engine = Rc::new(Recorder::default());
    let mailbox = Arc::new(Mailbox::default());
    let origin = Rc::new(Cell::new((0, 0)));
    let size = Rc::new(Cell::new((0, 0)));
    let (seen_origin, seen_size) = (Rc::clone(&origin), Rc::clone(&size));
    let (build_engine, build_mailbox, drive_engine) =
        (Rc::clone(&engine), Arc::clone(&mailbox), Rc::clone(&engine));
    let image = render_with(
        Snapshot::new(Dip(SIZE.0 as f32), Dip(SIZE.1 as f32)).theme(Theme::light()),
        move |ui| Browser::build(ui, build_engine, build_mailbox, None, Host::none()),
        move |stage: &Stage<'_, Msg>| {
            let top = Dip(38.0).to_px(stage.dpi()).0;
            seen_origin.set((0, top));
            let view = drive_engine
                .calls()
                .iter()
                .rev()
                .find_map(|c| match c {
                    Call::Resize(w, h) => Some((*w, *h)),
                    _ => None,
                })
                .expect("the engine was told the view size");
            seen_size.set(view);
            mailbox.set_scroll_offset(0, false);
            mailbox.post_frame(view.0 as u32, view.1 as u32, &rows(view.0, view.1, 0));
            stage.emit(Msg::Frame);
            script(stage, view, &mailbox, (view.0 / 2, top + view.1 / 2));
        },
    )
    .expect("the headless render");
    Shown {
        image,
        origin: origin.get(),
        size: size.get(),
    }
}

fn wheel(stage: &Stage<'_, Msg>, at: (i32, i32), notches: i32, modifiers: Modifiers) {
    stage.inject(Event::MouseWheel {
        delta: (notches * 120) as i16,
        horizontal: false,
        x: at.0,
        y: at.1,
        modifiers,
    });
}

#[test]
fn a_notch_moves_the_picture_at_once_and_leaves_a_checkerboard_below() {
    let shown = run(|stage, _, _, at| wheel(stage, at, -1, Modifiers::default()));
    // `XUI_SNAPSHOT=<dir>` keeps the picture to look at.
    if Gallery::from_env().dir().is_some() {
        Gallery::from_env()
            .save("scroll_ahead", "one_notch", &shown.image)
            .unwrap();
    }
    let (w, h) = shown.size;
    for y in [0, 1, 100, h / 2, h - STEP - 1] {
        assert_eq!(
            shown.at(10, y),
            row_colour(y + STEP),
            "row {y} is what was {STEP} rows lower"
        );
        assert_eq!(
            shown.at(w - 8, y),
            row_colour(y + STEP),
            "the page ends where the scroll bar begins"
        );
    }
    // The page's scroll bar is in its picture and stays where the page is: only the page moves it.
    for y in [0, 1, h / 2, h - STEP, h - 1] {
        for x in [w - 7, w - 4, w - 1] {
            assert_eq!(
                shown.at(x, y),
                row_colour(y),
                "scroll bar column {x} at row {y} did not move"
            );
        }
    }
    let mut seen = [false; 2];
    for y in (h - STEP)..h {
        for x in (0..w - 7).step_by(7) {
            let pixel = shown.at(x, y);
            assert!(
                pixel == LIGHT || pixel == DARK,
                "({x},{y}) is not a checkerboard square: {pixel:?}"
            );
            seen[usize::from(pixel == DARK)] = true;
        }
    }
    assert_eq!(seen, [true, true], "both kinds of square in the strip");
}

#[test]
fn up_from_the_top_is_not_shown_ahead() {
    // The page's offset is 0: there is nothing above it to uncover.
    let shown = run(|stage, _, _, at| wheel(stage, at, 1, Modifiers::default()));
    let h = shown.size.1;
    for y in [0, 1, h / 2, h - 1] {
        assert_eq!(shown.at(10, y), row_colour(y), "row {y} did not move");
    }
}

#[test]
fn a_modified_wheel_does_not_scroll_the_picture() {
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    let shown = run(move |stage, _, _, at| wheel(stage, at, -1, ctrl));
    let h = shown.size.1;
    for y in [0, 1, h / 2, h - 1] {
        assert_eq!(shown.at(10, y), row_colour(y), "row {y} did not move");
    }
}

#[test]
fn the_pages_answer_takes_the_picture_to_exact_and_no_checkerboard_is_left() {
    let shown = run(|stage, (w, h), mailbox, at| {
        wheel(stage, at, -1, Modifiers::default());
        // The page did that notch: the rows moved up, the rows it uncovered were painted.
        mailbox.set_scroll_offset(STEP, false);
        let delta = Delta {
            size: (w as u32, h as u32),
            scroll: Some(Scroll {
                area: IRect::new(0, 0, w, h),
                dx: 0,
                dy: -STEP,
            }),
            patches: vec![Patch {
                rect: IRect::new(0, h - STEP, w, STEP),
                rgba: solid(w, STEP, BLUE),
            }],
        };
        assert!(!mailbox.post_delta(delta), "the delta was queued");
        stage.emit(Msg::Frame);
    });
    let (w, h) = shown.size;
    for y in [0, 1, h / 2, h - STEP - 1] {
        assert_eq!(shown.at(10, y), row_colour(y + STEP), "row {y}");
    }
    for y in [h - STEP, h - 1] {
        for x in [0, w / 2, w - 1] {
            assert_eq!(
                shown.at(x, y),
                BLUE,
                "({x},{y}) is what the page painted, not a checkerboard"
            );
        }
    }
}

#[test]
fn with_smooth_scrolling_a_notch_starts_a_slide() {
    let shown = run_with(true, |stage, _, _, at| {
        wheel(stage, at, -1, Modifiers::default())
    });
    // The first frame of the slide: moved, but not yet the whole notch (the frame timer does the rest).
    let moved = (0..=STEP)
        .find(|&d| shown.at(10, 0) == row_colour(d))
        .expect("row 0 shows a page row");
    assert!(moved > 0 && moved < STEP, "moved {moved} of {STEP}");
}
