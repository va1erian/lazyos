//! A scroll delta drawn by the page view: what the window shows after the engine posted a frame and
//! then a delta that moved the page and repainted the rows it exposed and a scrollbar column. The
//! pixels are read back from the offscreen render, so the clips and offsets the view draws with are
//! what is judged (the composition rules are tested against a model in `scroll_model.rs`).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::{Dip, Image, Theme};
use xui_webkit::app::{Browser, Host, Msg};
use xui_webkit::damage::{IRect, Scroll};
use xui_webkit::engine::{Call, Recorder};
use xui_webkit::mailbox::{Delta, Mailbox, Patch};

const SIZE: (i32, i32) = (800, 500);
const BLUE: [u8; 3] = [0, 0, 255];
const RED: [u8; 3] = [255, 0, 0];

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
    let mut rgba = Vec::new();
    for y in 0..height {
        let c = row_colour(first_row + y);
        for _ in 0..width {
            rgba.extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    rgba
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

/// Posts a whole frame of banded rows, then (when `step` says so) a delta, and renders.
fn show(delta_of: impl FnOnce((i32, i32)) -> Option<Delta> + 'static) -> Shown {
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
            seen_origin.set((0, Dip(38.0).to_px(stage.dpi()).0));
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
            mailbox.post_frame(view.0 as u32, view.1 as u32, &rows(view.0, view.1, 0));
            if let Some(delta) = delta_of(view) {
                assert!(!mailbox.post_delta(delta), "the delta was queued");
            }
            stage.emit(Msg::Frame);
        },
    )
    .expect("the headless render");
    Shown {
        image,
        origin: origin.get(),
        size: size.get(),
    }
}

#[test]
fn a_frame_alone_shows_its_rows() {
    let shown = show(|_| None);
    let (w, h) = shown.size;
    for y in [0, 1, h / 2, h - 1] {
        assert_eq!(shown.at(w / 2, y), row_colour(y), "row {y}");
    }
}

#[test]
fn a_scroll_moves_the_rows_and_a_scrollbar_column_stays() {
    const STEP: i32 = 5;
    const BAR: i32 = 6;
    let shown = show(|(w, h)| {
        let area = IRect::new(0, 0, w - BAR, h);
        Some(Delta {
            size: (w as u32, h as u32),
            scroll: Some(Scroll {
                area,
                dx: 0,
                dy: -STEP,
            }),
            patches: vec![
                // The rows the scroll exposed, and the scrollbar, repainted.
                Patch {
                    rect: IRect::new(0, h - STEP, w - BAR, STEP),
                    rgba: solid(w - BAR, STEP, BLUE),
                },
                Patch {
                    rect: IRect::new(w - BAR, 0, BAR, h),
                    rgba: solid(BAR, h, RED),
                },
            ],
        })
    });
    let (w, h) = shown.size;
    for y in [0, 1, 100, h / 2, h - STEP - 1] {
        assert_eq!(
            shown.at(10, y),
            row_colour(y + STEP),
            "row {y} is what was {STEP} rows lower"
        );
        assert_eq!(
            shown.at(w - BAR - 1, y),
            row_colour(y + STEP),
            "row {y} at the area's right edge"
        );
    }
    for y in [h - STEP, h - 1] {
        assert_eq!(shown.at(10, y), BLUE, "row {y} is what the scroll exposed");
    }
    for y in [0, h / 2, h - 1] {
        assert_eq!(shown.at(w - 3, y), RED, "the scrollbar column at row {y}");
    }
}

#[test]
fn a_scroll_down_and_to_the_side_moves_the_right_way() {
    let shown = show(|(w, h)| {
        let area = IRect::new(0, 0, w, h);
        Some(Delta {
            size: (w as u32, h as u32),
            scroll: Some(Scroll { area, dx: 0, dy: 7 }),
            patches: vec![Patch {
                rect: IRect::new(0, 0, w, 7),
                rgba: solid(w, 7, BLUE),
            }],
        })
    });
    let (w, h) = shown.size;
    for y in [0, 6] {
        assert_eq!(
            shown.at(w / 2, y),
            BLUE,
            "row {y} is what the scroll exposed"
        );
    }
    for y in [7, 8, h / 2, h - 1] {
        assert_eq!(
            shown.at(w / 2, y),
            row_colour(y - 7),
            "row {y} is what was 7 rows higher"
        );
    }
}
