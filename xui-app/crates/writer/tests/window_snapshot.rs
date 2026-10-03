//! The whole LazyWriter window rendered offscreen, light and dark, with a
//! document that uses the formatting row: a heading, a bold word, an italic
//! word, a centred line, a list and a picture floated right. The PNGs land in
//! `xui-app/target/snapshots/writer-{light,dark}.png` for a human to look at.

mod common;

use std::sync::Arc;

use xui_canvas::snapshot::Stage;
use xui_core::{Dip, Image, Theme};
use xui_rich_text::DocPos;
use xui_rich_text::edit::Command;
use xui_rich_text::model::{Align, InlineImage, ListKind, OBJECT_CHAR, Side, Wrap};
use xui_writer::Msg;
use xui_writer::app::Mark;

use common::{Rig, TempDir, pump, render, save, watchdog};

/// The floated picture's colour, which nothing else in the window uses.
const RED: [u8; 4] = [230, 30, 30, 255];
/// The floated picture's size in design units (96 DPI: pixels).
const PICTURE: (u32, u32) = (160, 100);

const BODY: &str = "LazyWriter edits styled text with bold, italic, underlined and \
struck-through runs, paragraph alignment, lists and pictures that the text flows \
around, and it follows the desktop's light or dark theme. This sentence is here so \
the paragraph is long enough to wrap beside the floated picture on the right.";

fn picture() -> InlineImage {
    let (w, h) = PICTURE;
    let pixels = RED.repeat((w * h) as usize);
    InlineImage {
        image: Arc::new(Image::from_rgba(w, h, pixels).expect("pixels")),
        size: (Dip(w as f32), Dip(h as f32)),
        wrap: Wrap::square(Side::Right),
        alt: "red".into(),
    }
}

/// Fills the document through the same commands the toolbar runs, and leaves
/// the caret in the bold word when `caret_in_bold`, else in plain text.
fn fill(stage: &Stage<'_, Msg>, rig: &Rig, caret_in_bold: bool) {
    let e = &rig.editor;
    e.exec(Command::InsertText(format!(
        "My document\n{BODY}\nBullets keep their level\nNumbers count\nA centred closing line."
    )));
    let para = |i: usize| e.exec(Command::SelectParagraph(DocPos::new(i, 0)));
    para(0);
    stage.emit(Msg::Block(1));
    para(2);
    stage.emit(Msg::List(ListKind::Bullet));
    para(3);
    stage.emit(Msg::List(ListKind::Numbered));
    para(4);
    stage.emit(Msg::Align(Align::Center));
    let bold = BODY.find("bold").unwrap();
    e.exec(Command::SelectWord(DocPos::new(1, bold)));
    stage.emit(Msg::Toggle(Mark::Bold));
    e.exec(Command::SelectWord(DocPos::new(
        1,
        BODY.find("italic").unwrap(),
    )));
    stage.emit(Msg::Toggle(Mark::Italic));
    e.exec(Command::SetCaret {
        pos: DocPos::new(1, 0),
        extend: false,
    });
    e.exec(Command::InsertImage(picture()));
    let at = if caret_in_bold { bold + 2 } else { 2 };
    e.exec(Command::SetCaret {
        // The picture's anchor character now starts the paragraph.
        pos: DocPos::new(1, at + OBJECT_CHAR.len_utf8()),
        extend: false,
    });
    e.set_scroll(0.0);
    pump(stage);
}

/// Renders the filled window; also reports whether Bold showed as checked.
fn window(theme: Theme, caret_in_bold: bool) -> (Image, bool) {
    watchdog(move || {
        let dir = TempDir::new("snapshot");
        let checked = std::rc::Rc::new(std::cell::Cell::new(false));
        let seen = std::rc::Rc::clone(&checked);
        let image = render(theme, dir.0.clone(), move |stage, rig| {
            fill(stage, rig, caret_in_bold);
            seen.set(rig.bold.is_checked());
        });
        (image, checked.get())
    })
}

/// The pixels of `image` inside `(left, top, right, bottom)`.
fn region(image: &Image, (l, t, r, b): (u32, u32, u32, u32)) -> Vec<[u8; 4]> {
    (t..b)
        .flat_map(|y| (l..r).map(move |x| (x, y)))
        .filter_map(|(x, y)| image.pixel(x, y))
        .collect()
}

/// The bounding box of the pixels exactly `colour`, if any.
fn bounds_of(image: &Image, colour: [u8; 4]) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = image.size();
    let mut found: Option<(u32, u32, u32, u32)> = None;
    for y in 0..h {
        for x in 0..w {
            if image.pixel(x, y) == Some(colour) {
                let b = found.get_or_insert((x, y, x, y));
                *b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
            }
        }
    }
    found
}

// The formatting row (design units at 96 DPI): margins 8, spacing 4; block
// 120, family 96, size 64, a 6 spacer, then the 32-wide B / I / U / S toggles.
const BOLD_BUTTON: (u32, u32, u32, u32) = (310, 39, 342, 67);
const STRIKE_BUTTON: (u32, u32, u32, u32) = (418, 39, 450, 67);

#[test]
fn the_window_renders_in_light_and_dark() {
    let (light, light_bold) = window(Theme::light(), true);
    let (dark, dark_bold) = window(Theme::dark(), true);
    save(&light, "writer-light.png");
    save(&dark, "writer-dark.png");
    assert_eq!(light.size(), dark.size());
    assert_eq!(light.size(), (common::WIDTH as u32, common::HEIGHT as u32));
    assert!(light_bold && dark_bold, "the caret sits in a bold word");
    // The document's empty margin follows the theme: light is light, dark dark.
    let luma = |p: [u8; 4]| u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]);
    let (l, d) = (light.pixel(4, 600).unwrap(), dark.pixel(4, 600).unwrap());
    assert!(luma(l) > 600, "light document background {l:?}");
    assert!(luma(d) < 200, "dark document background {d:?}");
}

#[test]
fn the_bold_toggle_shows_checked_in_a_bold_word() {
    let (bold, checked) = window(Theme::light(), true);
    let (plain, unchecked) = window(Theme::light(), false);
    save(&plain, "writer-light-plain-caret.png");
    assert!(checked && !unchecked);
    assert_ne!(
        region(&bold, BOLD_BUTTON),
        region(&plain, BOLD_BUTTON),
        "a checked Bold toggle is drawn differently"
    );
    assert_eq!(
        region(&bold, STRIKE_BUTTON),
        region(&plain, STRIKE_BUTTON),
        "Strike is off in both"
    );
}

#[test]
fn the_floated_picture_sits_at_the_right_of_the_text() {
    let (image, _) = window(Theme::light(), true);
    let (left, top, right, bottom) = bounds_of(&image, RED).expect("the picture is drawn");
    let (w, h) = PICTURE;
    assert_eq!(
        (right - left + 1, bottom - top + 1),
        (w, h),
        "drawn at its size"
    );
    assert!(
        left > common::WIDTH as u32 / 2,
        "floated right, at x {left}"
    );
    assert!(
        (70..300).contains(&top),
        "beside the first paragraphs, at y {top}"
    );
}
