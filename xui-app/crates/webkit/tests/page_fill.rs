//! What the page view shows under and around a frame (`view.rs`): a frame that covers the view is
//! not painted over white, a smaller one sits at the top left over white (never over the theme),
//! and a page whose process is gone is white. Driven offscreen against a recording engine, like
//! `window.rs`.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use xui_canvas::snapshot::{render_with, Snapshot, Stage};
use xui_core::{Dip, Image, Theme};
use xui_webkit::app::{Browser, Host, Msg};
use xui_webkit::engine::{Call, Recorder};
use xui_webkit::mailbox::{Mailbox, PageState};

const SIZE: (i32, i32) = (800, 500);

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

struct Rig {
    engine: Rc<Recorder>,
    mailbox: Arc<Mailbox>,
}

fn drive(step: impl FnOnce(&Stage<'_, Msg>, &Rig) + 'static) -> Image {
    register_fonts();
    let engine = Rc::new(Recorder::default());
    let mailbox = Arc::new(Mailbox::default());
    let rig = Rig {
        engine: Rc::clone(&engine),
        mailbox: Arc::clone(&mailbox),
    };
    let (build_engine, build_mailbox) = (Rc::clone(&engine), Arc::clone(&mailbox));
    render_with(
        Snapshot::new(Dip(SIZE.0 as f32), Dip(SIZE.1 as f32)).theme(Theme::light()),
        move |ui| Browser::build(ui, build_engine, build_mailbox, None, Host::none()),
        move |stage| step(stage, &rig),
    )
    .expect("the headless render")
}

/// The page view's origin in the window: below the toolbar.
fn page_origin(stage: &Stage<'_, Msg>) -> (i32, i32) {
    (0, Dip(38.0).to_px(stage.dpi()).0)
}

/// An opaque picture of one colour.
fn solid(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..width * height {
        rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }
    rgba
}

const GREEN: [u8; 3] = [0, 160, 0];
const WHITE: [u8; 3] = [255, 255, 255];

/// What the page view shows once the engine has posted a frame of `frame`'s size: the view's
/// origin and size, the window's pixels, and a way to read one view pixel. `frame` gets the view's
/// own size (as the engine was last told it) when it returns `None`; `crash` ends with the
/// page's process gone.
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

fn shown(frame: Option<(u32, u32)>, crash: bool) -> Shown {
    let origin = Rc::new(Cell::new((0, 0)));
    let size = Rc::new(Cell::new((0, 0)));
    let (seen_origin, seen_size) = (Rc::clone(&origin), Rc::clone(&size));
    let image = drive(move |stage, rig| {
        seen_origin.set(page_origin(stage));
        let view = rig
            .engine
            .calls()
            .iter()
            .rev()
            .find_map(|c| match c {
                Call::Resize(w, h) => Some((*w, *h)),
                _ => None,
            })
            .expect("the engine was told the view size");
        seen_size.set(view);
        let (w, h) = frame.unwrap_or((view.0 as u32, view.1 as u32));
        rig.mailbox.post_frame(w, h, &solid(w, h, GREEN));
        stage.emit(Msg::Frame);
        if crash {
            rig.mailbox.post_state(PageState {
                crashed: true,
                ..PageState::default()
            });
            stage.emit(Msg::State);
        }
    });
    Shown {
        image,
        origin: origin.get(),
        size: size.get(),
    }
}

#[test]
fn a_frame_that_covers_the_view_is_not_painted_over_white() {
    let equal = shown(None, false);
    let (w, h) = equal.size;
    assert_eq!(equal.at(0, 0), GREEN);
    assert_eq!(
        equal.at(w - 1, h - 1),
        GREEN,
        "the whole view is the picture"
    );

    // Larger than the view (a window that shrank before the next frame): clipped, no white.
    let larger = shown(Some((w as u32 + 40, h as u32 + 40)), false);
    assert_eq!(larger.at(0, 0), GREEN);
    assert_eq!(larger.at(w - 1, h - 1), GREEN);
}

#[test]
fn a_frame_smaller_than_the_view_leaves_white_not_the_theme_behind_it() {
    let small = shown(Some((300, 200)), false);
    let (w, h) = small.size;
    assert!(w > 400 && h > 250, "the view is {w}x{h}");
    assert_eq!(
        small.at(10, 10),
        GREEN,
        "the picture sits at the top left, never stretched"
    );
    assert_eq!(small.at(299, 199), GREEN);
    assert_eq!(small.at(w - 5, 100), WHITE, "right of the picture");
    assert_eq!(small.at(100, h - 5), WHITE, "below the picture");
    assert_eq!(small.at(w - 5, h - 5), WHITE);
}

#[test]
fn a_blanked_page_is_white_whatever_frame_was_there() {
    let crashed = shown(None, true);
    let (w, h) = crashed.size;
    assert_eq!(crashed.at(5, 5), WHITE);
    assert_eq!(crashed.at(w / 2, h / 2), WHITE);
    assert_eq!(crashed.at(w - 1, h - 1), WHITE);
}
