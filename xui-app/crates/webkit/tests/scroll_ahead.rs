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
mod support;
use support::{Browser, Host, Msg};
use xui_webkit::damage::{IRect, Scroll};
use xui_webkit::engine::{Call, Recorder};
use xui_webkit::mailbox::{Delta, Mailbox, PageState, Patch};

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
    // These tests are of the picture shown ahead, off by default since #940.
    run_mode(smooth, true, script)
}

/// `run`, with smooth scrolling and showing the picture ahead each on or off.
fn run_mode(
    smooth: bool,
    ahead: bool,
    script: impl FnOnce(&Stage<'_, Msg>, (i32, i32), &Mailbox, (i32, i32)) + 'static,
) -> Shown {
    xui_webkit::glide::set_smooth(smooth);
    xui_webkit::speculate::set_ahead(ahead);
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

/// The page did one notch: its rows moved up, and it painted the rows it uncovered blue.
fn answer_one_notch(stage: &Stage<'_, Msg>, (w, h): (i32, i32), mailbox: &Mailbox) {
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
}

#[test]
fn without_showing_ahead_a_notch_waits_for_the_page() {
    // The default since #940: nothing moves, no checkerboard, until the page has scrolled.
    let shown = run_mode(false, false, |stage, _, _, at| {
        wheel(stage, at, -1, Modifiers::default())
    });
    let h = shown.size.1;
    for y in [0, 1, h / 2, h - STEP, h - 1] {
        assert_eq!(
            shown.at(10, y),
            row_colour(y),
            "row {y} moved before the page did"
        );
    }
}

#[test]
fn without_showing_ahead_the_pages_answer_moves_the_view() {
    let shown = run_mode(false, false, |stage, view, mailbox, at| {
        wheel(stage, at, -1, Modifiers::default());
        answer_one_notch(stage, view, mailbox);
    });
    let (w, h) = shown.size;
    for y in [0, 1, h / 2, h - STEP - 1] {
        assert_eq!(shown.at(10, y), row_colour(y + STEP), "row {y}");
    }
    for y in [h - STEP, h - 1] {
        for x in [0, w / 2, w - 1] {
            assert_eq!(shown.at(x, y), BLUE, "({x},{y}) is what the page painted");
        }
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

/// The page's answer to a notch it could only do part of, the page's end: it scrolled `moved` rows (its
/// rows move up, the rows it uncovered are painted) and says it is at the bottom.
fn answer_at_the_end(
    stage: &Stage<'_, Msg>,
    (w, h): (i32, i32),
    mailbox: &Mailbox,
    offset: i32,
    moved: i32,
) {
    mailbox.set_scroll_offset(offset, true);
    let delta = Delta {
        size: (w as u32, h as u32),
        scroll: Some(Scroll {
            area: IRect::new(0, 0, w, h),
            dx: 0,
            dy: -moved,
        }),
        patches: vec![Patch {
            rect: IRect::new(0, h - moved, w, moved),
            rgba: solid(w, moved, BLUE),
        }],
    };
    assert!(!mailbox.post_delta(delta), "the delta was queued");
    stage.emit(Msg::Frame);
}

#[test]
fn at_the_end_of_the_page_with_no_bands_what_was_shown_past_it_is_given_back() {
    // Three notches asked, 120 rows ahead; the page had 40 rows left to scroll and said it is at the bottom.
    let shown = run(|stage, size, mailbox, at| {
        wheel(stage, at, -3, Modifiers::default());
        answer_at_the_end(stage, size, mailbox, STEP, STEP);
        // The wheel keeps turning against the end: nothing is shown ahead any more.
        wheel(stage, at, -3, Modifiers::default());
        wheel(stage, at, -3, Modifiers::default());
    });
    let (w, h) = shown.size;
    for y in [0, 1, h / 2, h - STEP - 1] {
        assert_eq!(
            shown.at(10, y),
            row_colour(y + STEP),
            "row {y} is where the page is"
        );
    }
    for y in [h - STEP, h - 1] {
        for x in [0, w / 2, w - 9] {
            assert_eq!(
                shown.at(x, y),
                BLUE,
                "({x},{y}) is the page's last rows, not a checkerboard"
            );
        }
    }
}

#[test]
fn a_new_page_is_shown_from_its_own_top_after_scrolling_the_old_one() {
    // Smooth scrolling on: the picture glides, from a row the old page reached.
    let shown = run_with(true, |stage, (w, h), mailbox, at| {
        for _ in 0..3 {
            wheel(stage, at, -1, Modifiers::default());
        }
        mailbox.set_scroll_offset(3 * STEP, false);
        mailbox.post_frame(w as u32, h as u32, &rows(w, h, 3 * STEP));
        stage.emit(Msg::Frame);
        // A link: another document, at its top.
        mailbox.post_state(PageState {
            committed_url: "https://b.test/".into(),
            commits: 1,
            ..PageState::default()
        });
        stage.emit(Msg::State);
        mailbox.set_scroll_offset(0, false);
        mailbox.post_frame(w as u32, h as u32, &solid(w, h, [0, 0, 255]));
        stage.emit(Msg::Frame);
    });
    let (w, h) = shown.size;
    for y in [0, 1, h / 2, h - 1] {
        assert_eq!(
            shown.at(w / 3, y),
            [0, 0, 255],
            "row {y} is the new page's own, not the old one's row or a checkerboard"
        );
    }
}

#[test]
fn a_late_answer_from_the_old_page_does_not_move_the_new_one() {
    let shown = run_with(true, |stage, (w, h), mailbox, at| {
        for _ in 0..3 {
            wheel(stage, at, -1, Modifiers::default());
        }
        mailbox.post_state(PageState {
            committed_url: "https://b.test/".into(),
            commits: 1,
            ..PageState::default()
        });
        stage.emit(Msg::State);
        mailbox.set_scroll_offset(0, false);
        mailbox.post_frame(w as u32, h as u32, &solid(w, h, [0, 0, 255]));
        stage.emit(Msg::Frame);
        // The old page answered turn 3 just before it went: far down, where it was.
        mailbox.wheel_done(3, 3000);
        stage.emit(Msg::Frame);
    });
    let (w, h) = shown.size;
    for y in [0, 1, h / 2, h - 1] {
        assert_eq!(
            shown.at(w / 3, y),
            [0, 0, 255],
            "row {y} is the new page, not shown ahead of it over a checkerboard"
        );
    }
}

#[test]
fn without_showing_ahead_an_answer_moves_nothing() {
    let shown = run_mode(false, false, |stage, _, mailbox, at| {
        wheel(stage, at, -1, Modifiers::default());
        // The page answered (it is one notch down) before its frame came.
        mailbox.wheel_done(1, STEP);
        stage.emit(Msg::Frame);
    });
    let h = shown.size.1;
    for y in [0, 1, h / 2, h - 1] {
        assert_eq!(
            shown.at(10, y),
            row_colour(y),
            "row {y} moved before the page's frame"
        );
    }
}

#[test]
fn a_reload_of_the_same_address_starts_from_the_new_documents_top() {
    let shown = run_with(true, |stage, (w, h), mailbox, at| {
        for _ in 0..3 {
            wheel(stage, at, -1, Modifiers::default());
        }
        // The reload commits a new document under the same (empty) address.
        mailbox.post_state(PageState {
            commits: 1,
            ..PageState::default()
        });
        stage.emit(Msg::State);
        mailbox.set_scroll_offset(0, false);
        mailbox.post_frame(w as u32, h as u32, &solid(w, h, [0, 0, 255]));
        stage.emit(Msg::Frame);
        // The old document's answer, on its way across the reload.
        mailbox.wheel_done(3, 3000);
        stage.emit(Msg::Frame);
    });
    let (w, h) = shown.size;
    for y in [0, 1, h / 2, h - 1] {
        assert_eq!(
            shown.at(w / 3, y),
            [0, 0, 255],
            "row {y} is the reloaded page's own"
        );
    }
}
