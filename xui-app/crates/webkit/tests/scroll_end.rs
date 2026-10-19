//! The end of the page with bands below the view, as pixels. Its own test binary: the bands the engine
//! posts wait in one queue for the whole process, and the window of any other test would take them.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::backend::Event;
use xui_core::{Dip, Image, Modifiers, Theme};
use xui_webkit::app::{Browser, Host, Msg};
use xui_webkit::damage::{IRect, Scroll};
use xui_webkit::engine::{Call, Recorder};
use xui_webkit::mailbox::{Delta, Mailbox, Patch};

const SIZE: (i32, i32) = (800, 500);
const BLUE: [u8; 3] = [0, 0, 255];
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

const BAND: [u8; 3] = [200, 30, 30];
/// Rows the engine paints below the view at the page's end (29 at webkit.org at 2560x1440).
const MARGIN: i32 = 29;

#[test]
fn at_the_end_of_the_page_a_margin_of_bands_below_the_view_is_not_shown_ahead() {
    // The page did the notch it had left (40 rows), said it is pinned to the bottom, and its process
    // painted a margin of rows below the view. The wheel keeps turning against the end: the old rule
    // took the picture 480 rows ahead (the margin, then a checkerboard); nothing moves now.
    let shown = run(|stage, (w, h), mailbox, at| {
        mailbox.set_scroll_offset(STEP, true);
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
        xui_webkit::bands::post(xui_webkit::bands::Band {
            doc_x: 0,
            doc_y: STEP + h,
            width: w as u32,
            height: MARGIN as u32,
            rgba: solid(w, MARGIN, BAND),
        });
        stage.emit(Msg::Frame);
        for _ in 0..6 {
            wheel(stage, at, -3, Modifiers::default());
        }
    });
    let (w, h) = shown.size;
    for y in [0, 1, h / 2, h - STEP - 1] {
        assert_eq!(shown.at(10, y), row_colour(y + STEP), "row {y} is where the page is");
    }
    for y in [h - STEP, h - MARGIN - 1, h - 1] {
        for x in [0, w / 2, w - 9] {
            assert_eq!(shown.at(x, y), BLUE, "({x},{y}) is the page's last rows: nothing is shown past them");
        }
    }
}
