//! Open and Save for every format through a path-aware in-memory store, the
//! "Flatten image?" question, the unsaved-changes guard, the title and the
//! evidence the app reports (`PAINT:OPEN:PASS`, `PAINT:SAVE:PASS`).

mod common;

use std::path::PathBuf;
use std::rc::Rc;

use xui_core::message::Key;
use xui_paint::Msg;
use xui_paint::model::{Bitmap, Format, Model, open_file};
use xui_paint::view::Report;

use common::files::*;

const TINY_JPG: &[u8] = include_bytes!("fixtures/tiny.jpg");

fn png(width: u32, height: u32) -> Vec<u8> {
    Bitmap::white(width, height).encode_png().unwrap()
}

fn two_layer_model(width: u32, height: u32) -> Model {
    let mut model = Model::new(width, height);
    model.add_layer().unwrap();
    model
}

fn ora(width: u32, height: u32) -> Vec<u8> {
    two_layer_model(width, height)
        .save_bytes(Format::OpenRaster)
        .unwrap()
}

fn bmp(width: u32, height: u32) -> Vec<u8> {
    Model::new(width, height).save_bytes(Format::Bmp).unwrap()
}

/// Opens `name` through Ctrl+O and the picker.
fn open_file_named(stage: &Stg<'_>, name: &str) {
    chord(stage, Key::O, false);
    type_text(stage, name);
    key(stage, Key::RETURN);
}

/// Saves under `name` through Ctrl+Shift+S and the picker.
fn save_as(stage: &Stg<'_>, name: &str) {
    chord(stage, Key::S, true);
    replace_text(stage, name);
    key(stage, Key::RETURN);
}

/// The document a file on the disk decodes to.
fn read_back(disk: &Disk, path: &str) -> (Format, xui_paint::model::Document) {
    open_file(
        &disk
            .get(path)
            .unwrap_or_else(|| panic!("{path} was written")),
    )
    .unwrap()
}

fn add_layer(stage: &Stg<'_>) {
    chord(stage, Key::N, true);
}

fn stroke(stage: &Stg<'_>, ctx: &Ctx) {
    drag_picture(stage, ctx, (60, 40), (90, 40));
}

#[test]
fn open_reads_png_bmp_ora_and_jpeg_and_reports_each() {
    let disk = Disk::with(&[
        ("/a.png", png(50, 30)),
        ("/b.bmp", bmp(40, 20)),
        ("/c.ora", ora(60, 45)),
        ("/d.jpg", TINY_JPG.to_vec()),
    ]);
    session(&disk, |stage, ctx| {
        for (name, size, layers) in [
            ("a.png", "50 x 30", 1),
            ("b.bmp", "40 x 20", 1),
            ("c.ora", "60 x 45", 2),
            ("d.jpg", "24 x 16", 1),
        ] {
            open_file_named(stage, name);
            assert_eq!(ctx.status(), format!("Opened {name}"), "{name}");
            assert_eq!(ctx.size(), size, "{name}");
            assert_eq!(ctx.layers(), layers, "{name}");
            assert_eq!(ctx.title(), format!("{name} - Paint"), "{name}");
            assert!(!ctx.dirty(), "{name} opens clean");
            assert_eq!(ctx.history(), (0, 0), "{name} starts a fresh history");
        }
        let reports = ctx.reports.borrow();
        let want = |path: &str, size, layers| Report::Opened {
            path: Some(PathBuf::from(path)),
            size,
            layers,
        };
        assert_eq!(
            *reports,
            vec![
                want("/a.png", (50, 30), 1),
                want("/b.bmp", (40, 20), 1),
                want("/c.ora", (60, 45), 2),
                want("/d.jpg", (24, 16), 1),
            ]
        );
    });
}

#[test]
fn a_file_that_is_not_a_picture_is_refused_and_the_canvas_kept() {
    let disk = Disk::with(&[
        ("/x.png", b"hello".to_vec()),
        ("/y.ora", b"PK\x03\x04junk".to_vec()),
    ]);
    session(&disk, |stage, ctx| {
        for name in ["x.png", "y.ora"] {
            open_file_named(stage, name);
            assert!(ctx.status().starts_with("Open failed"), "{}", ctx.status());
            assert_eq!(ctx.size(), "320 x 240");
        }
        assert!(ctx.reports.borrow().is_empty(), "no PASS marker on failure");
    });
}

#[test]
fn save_writes_each_format_by_the_extension() {
    for (name, format) in [
        ("pic.png", Format::Png),
        ("pic.bmp", Format::Bmp),
        ("pic.ora", Format::OpenRaster),
    ] {
        let disk = Rc::new(Disk::default());
        session(&disk, move |stage, ctx| {
            stroke(stage, ctx);
            save_as(stage, name);
            assert_eq!(ctx.status(), format!("Saved {name}"));
            assert_eq!(ctx.title(), format!("{name} - Paint"));
            assert!(!ctx.dirty());
            assert_eq!(
                ctx.reports.borrow().last(),
                Some(&Report::Saved {
                    path: Some(PathBuf::from(format!("/{name}"))),
                    format
                })
            );
            let (found, doc) = read_back(&ctx.disk, &format!("/{name}"));
            assert_eq!(found, format);
            assert_eq!(doc.size(), (320, 240));
        });
    }
}

#[test]
fn save_as_suggests_ora_for_layers_and_png_otherwise() {
    let disk = Rc::new(Disk::default());
    session(&disk, |stage, ctx| {
        // One layer: Enter takes the suggestion, untitled.png.
        chord(stage, Key::S, true);
        key(stage, Key::RETURN);
        assert_eq!(ctx.status(), "Saved untitled.png");
        assert_eq!(read_back(&ctx.disk, "/untitled.png").0, Format::Png);
    });
    let disk = Rc::new(Disk::default());
    session(&disk, |stage, ctx| {
        add_layer(stage);
        chord(stage, Key::S, true);
        key(stage, Key::RETURN);
        assert_eq!(ctx.status(), "Saved untitled.ora");
        let (format, doc) = read_back(&ctx.disk, "/untitled.ora");
        assert_eq!(format, Format::OpenRaster);
        assert_eq!(doc.layers().len(), 2, "the layers are in the file");
    });
}

#[test]
fn a_name_without_an_extension_gets_one_for_the_picture() {
    let disk = Rc::new(Disk::default());
    session(&disk, |stage, ctx| {
        save_as(stage, "plain");
        assert_eq!(ctx.status(), "Saved plain.png");
    });
    let disk = Rc::new(Disk::default());
    session(&disk, |stage, ctx| {
        add_layer(stage);
        save_as(stage, "layered");
        assert_eq!(ctx.status(), "Saved layered.ora");
    });
}

#[test]
fn saving_layers_to_png_or_bmp_asks_to_flatten_and_flattens_only_the_file() {
    for (name, format) in [("flat.png", Format::Png), ("flat.bmp", Format::Bmp)] {
        let disk = Rc::new(Disk::default());
        session(&disk, move |stage, ctx| {
            add_layer(stage);
            assert_eq!(ctx.layers(), 2);
            save_as(stage, name);
            assert_eq!(ctx.disk.get(&format!("/{name}")), None, "asked first");
            assert!(ctx.dirty(), "still unsaved while the card is up");
            key(stage, Key::RETURN); // Flatten
            assert_eq!(ctx.status(), format!("Saved {name}"));
            let (found, doc) = read_back(&ctx.disk, &format!("/{name}"));
            assert_eq!(found, format);
            assert_eq!(doc.layers().len(), 1, "the file is flat");
            assert_eq!(ctx.layers(), 2, "the picture keeps its layers");
            assert_eq!(ctx.title(), format!("{name} - Paint"));
        });
    }
}

#[test]
fn declining_to_flatten_writes_nothing() {
    let disk = Rc::new(Disk::default());
    session(&disk, |stage, ctx| {
        add_layer(stage);
        save_as(stage, "flat.png");
        key(stage, Key::ESCAPE);
        assert!(ctx.disk.files.borrow().is_empty());
        assert!(ctx.dirty());
        assert_eq!(ctx.title(), "untitled* - Paint");
        // Layers in an OpenRaster file need no question.
        save_as(stage, "kept.ora");
        assert_eq!(ctx.status(), "Saved kept.ora");
    });
}

#[test]
fn jpeg_cannot_be_saved_and_says_so() {
    let disk = Rc::new(Disk::default());
    session(&disk, |stage, ctx| {
        stroke(stage, ctx);
        save_as(stage, "photo.jpg");
        let status = ctx.status();
        assert!(
            status.contains("JPEG") && status.contains(".png"),
            "{status}"
        );
        assert!(ctx.disk.files.borrow().is_empty());
        assert!(ctx.dirty());
        save_as(stage, "photo.jpeg");
        assert!(ctx.status().contains("JPEG"));
    });
}

#[test]
fn save_after_opening_a_jpeg_goes_through_save_as_with_a_png_name() {
    let disk = Disk::with(&[("/d.jpg", TINY_JPG.to_vec())]);
    session(&disk, |stage, ctx| {
        open_file_named(stage, "d.jpg");
        stroke(stage, ctx);
        chord(stage, Key::S, false);
        assert!(ctx.status().contains("JPEG"), "{}", ctx.status());
        key(stage, Key::RETURN);
        assert_eq!(
            ctx.status(),
            "Saved d.png",
            "the suggestion swaps the extension"
        );
        assert_eq!(ctx.title(), "d.png - Paint");
    });
}

#[test]
fn an_ora_round_trips_through_save_and_open() {
    let disk = Rc::new(Disk::default());
    session(&disk, |stage, ctx| {
        add_layer(stage);
        add_layer(stage);
        save_as(stage, "three.ora");
        chord(stage, Key::N, false); // New: clean, so no question
        assert_eq!(ctx.layers(), 1);
        open_file_named(stage, "three.ora");
        assert_eq!(ctx.layers(), 3);
        assert_eq!(ctx.size(), "320 x 240");
    });
}

#[test]
fn the_title_follows_the_document_and_its_unsaved_state() {
    let disk = Disk::with(&[("/a.png", png(50, 30))]);
    session(&disk, |stage, ctx| {
        assert_eq!(ctx.title(), "untitled - Paint");
        assert!(!ctx.dirty());
        stroke(stage, ctx);
        assert_eq!(ctx.title(), "untitled* - Paint");
        save_as(stage, "t.png");
        assert_eq!(ctx.title(), "t.png - Paint");
        stroke(stage, ctx);
        assert_eq!(ctx.title(), "t.png* - Paint");
        chord(stage, Key::S, false);
        assert_eq!(ctx.title(), "t.png - Paint", "Save writes there and cleans");
        // A picture opened from a file starts clean; a selection is a history
        // step, like in Paint.NET, so it counts as a change.
        open_file_named(stage, "a.png");
        assert_eq!(ctx.title(), "a.png - Paint");
        chord(stage, Key::A, false);
        assert_eq!(ctx.title(), "a.png* - Paint");
    });
}

#[test]
fn new_open_and_quit_ask_before_dropping_unsaved_changes() {
    let disk = Disk::with(&[("/a.png", png(50, 30))]);
    session(&disk, |stage, ctx| {
        stroke(stage, ctx);
        // New: the card is up, Esc keeps the picture.
        chord(stage, Key::N, false);
        assert_eq!(ctx.history(), (1, 1), "waiting for the answer");
        key(stage, Key::ESCAPE);
        assert_eq!(ctx.history(), (1, 1), "Cancel kept it");
        assert!(ctx.dirty());

        // Open: Cancel keeps it, Dont Save goes to the picker.
        chord(stage, Key::O, false);
        key(stage, Key::ESCAPE);
        assert_eq!(ctx.size(), "320 x 240");
        chord(stage, Key::O, false);
        dont_save(stage);
        type_text(stage, "a.png");
        key(stage, Key::RETURN);
        assert_eq!(ctx.size(), "50 x 30");

        // Quit with unsaved changes asks; Cancel keeps the window.
        stroke_small(stage, ctx);
        chord(stage, Key::Q, false);
        key(stage, Key::ESCAPE);
        assert!(ctx.dirty(), "still here");
    });
}

fn stroke_small(stage: &Stg<'_>, ctx: &Ctx) {
    drag_picture(stage, ctx, (10, 10), (20, 10));
}

#[test]
fn dont_save_drops_the_changes_and_does_what_was_asked() {
    let disk = Rc::new(Disk::default());
    session(&disk, |stage, ctx| {
        stroke(stage, ctx);
        chord(stage, Key::N, false);
        dont_save(stage);
        assert_eq!(ctx.history(), (0, 0));
        assert!(!ctx.dirty());
        assert_eq!(ctx.title(), "untitled - Paint");
        assert!(ctx.disk.files.borrow().is_empty(), "nothing was written");
    });
}

#[test]
fn save_in_the_card_saves_then_goes_on() {
    let disk = Rc::new(Disk::default());
    session(&disk, |stage, ctx| {
        stroke(stage, ctx);
        chord(stage, Key::N, false);
        key(stage, Key::RETURN); // Save: untitled, so the picker comes up
        replace_text(stage, "guarded.png");
        key(stage, Key::RETURN);
        assert!(ctx.disk.get("/guarded.png").is_some(), "saved first");
        assert_eq!(ctx.history(), (0, 0), "then New ran");
        assert_eq!(ctx.title(), "untitled - Paint");
    });
}

#[test]
fn a_cancelled_picker_or_a_failed_write_cancels_what_the_card_was_for() {
    let disk = Rc::new(Disk::default());
    session(&disk, |stage, ctx| {
        stroke(stage, ctx);
        chord(stage, Key::N, false);
        key(stage, Key::RETURN); // Save: the picker comes up
        key(stage, Key::ESCAPE); // and is cancelled
        assert_eq!(ctx.history(), (1, 1), "New did not run");

        // A failed write also cancels it, and the picture stays unsaved.
        ctx.disk.fail_writes.set(true);
        chord(stage, Key::N, false);
        key(stage, Key::RETURN);
        key(stage, Key::RETURN); // the picker accepts untitled.png
        assert!(ctx.status().starts_with("Save failed"), "{}", ctx.status());
        assert_eq!(ctx.history(), (1, 1));
        assert!(ctx.dirty());
        ctx.disk.fail_writes.set(false);
    });
}

#[test]
fn a_clean_picture_is_replaced_without_a_question() {
    let disk = Disk::with(&[("/a.png", png(50, 30))]);
    session(&disk, |stage, ctx| {
        open_file_named(stage, "a.png");
        chord(stage, Key::N, false);
        assert_eq!(ctx.size(), "320 x 240", "New ran at once");
        assert_eq!(ctx.title(), "untitled - Paint");
        stage.emit(Msg::NewTransparent);
        assert_eq!(ctx.history(), (0, 0));
    });
}
