#![forbid(unsafe_code)]

//! The scripted self-test: a doodle through the model plus the storage
//! contract, printing PASS/FAIL to a writer.
//!
//! This is what runs on a host where `cargo test` is unavailable (a toy OS);
//! `main.rs` adds the headless offscreen render on top when the `canvas`
//! feature is on, and `tests/selftest.rs` runs the same checks under
//! `cargo test`.

use std::io::Write;

use crate::model::{Bitmap, History, MAX_SIDE, Model, Pixel, Side, Tool, WHITE};
use crate::storage::{FailingStorage, MemoryStorage, Storage};

const RED: Pixel = [255, 0, 0, 255];

/// Runs every model and storage check, writing one line per check.
///
/// Returns `true` only when all checks passed.
pub fn run(out: &mut dyn Write) -> bool {
    let mut all = true;
    let mut check = |name: &str, ok: bool, detail: &str| {
        if ok {
            let _ = writeln!(out, "PASS {name}");
        } else {
            let _ = writeln!(out, "FAIL {name}: {detail}");
            all = false;
        }
    };

    let fresh = Model::new(320, 240);
    check(
        "new canvas is white at the default size",
        fresh.bitmap().size() == (320, 240) && fresh.bitmap().get(0, 0) == Some(WHITE),
        "the fresh bitmap was not white 320x240",
    );

    check(
        "pencil draws an unbroken line",
        {
            let mut model = Model::new(32, 32);
            model.set_primary(RED);
            model.begin(0, 0, Side::Primary);
            model.extend(31, 31);
            model.end();
            (0..32).all(|i| model.bitmap().get(i, i) == Some(RED))
        },
        "the pencil line left a gap",
    );

    check(
        "a drag is a single undo step",
        {
            let mut model = Model::new(32, 32);
            model.begin(0, 0, Side::Primary);
            model.extend(5, 0);
            model.extend(10, 0);
            model.end();
            let steps = model.history().undo_len();
            model.undo();
            steps == 1 && model.bitmap().get(5, 0) == Some(WHITE)
        },
        "a drag did not undo as one step",
    );

    check(
        "the brush is wider than the pencil",
        {
            let mut thin = Model::new(32, 32);
            thin.set_tool(Tool::Pencil);
            thin.begin(8, 16, Side::Primary);
            thin.end();
            let mut thick = Model::new(32, 32);
            thick.set_tool(Tool::Brush);
            thick.set_size(8);
            thick.begin(8, 16, Side::Primary);
            thick.end();
            lit(thick.bitmap()) > lit(thin.bitmap())
        },
        "the brush painted no more than the pencil",
    );

    check(
        "rectangles and ellipses are hollow outlines",
        {
            let mut model = Model::new(32, 32);
            model.set_tool(Tool::Rectangle);
            model.set_primary(RED);
            model.begin(2, 2, Side::Primary);
            model.extend(28, 28);
            model.end();
            let rect_ok =
                model.bitmap().get(2, 2) == Some(RED) && model.bitmap().get(15, 15) == Some(WHITE);
            model.set_tool(Tool::Ellipse);
            model.begin(2, 2, Side::Primary);
            model.extend(28, 28);
            model.end();
            let ellipse_ok = model.bitmap().get(15, 15) == Some(WHITE);
            rect_ok && ellipse_ok
        },
        "a shape was filled or missing",
    );

    check(
        "flood fill stays inside a wall",
        {
            let mut model = Model::new(16, 16);
            model.set_tool(Tool::Line);
            model.begin(8, 0, Side::Primary);
            model.extend(8, 15);
            model.end();
            model.set_tool(Tool::Fill);
            model.set_primary(RED);
            model.begin(0, 0, Side::Primary);
            model.bitmap().get(3, 3) == Some(RED) && model.bitmap().get(12, 3) == Some(WHITE)
        },
        "the fill crossed a wall",
    );

    check(
        "the eraser paints the background",
        {
            let mut model = Model::new(16, 16);
            model.begin(4, 4, Side::Primary);
            model.end();
            model.set_tool(Tool::Eraser);
            model.begin(4, 4, Side::Primary);
            model.end();
            model.bitmap().get(4, 4) == Some(WHITE)
        },
        "the eraser left paint behind",
    );

    check(
        "the eyedropper samples the canvas",
        {
            let mut model = Model::new(16, 16);
            model.set_primary(RED);
            model.begin(4, 4, Side::Primary);
            model.end();
            model.set_tool(Tool::Picker);
            model.begin(4, 4, Side::Secondary);
            model.secondary() == RED
        },
        "the eyedropper did not pick the colour",
    );

    check(
        "redo is cleared by a new action",
        {
            let mut model = Model::new(16, 16);
            model.begin(1, 1, Side::Primary);
            model.end();
            model.undo();
            let had_redo = model.history().can_redo();
            model.begin(2, 2, Side::Primary);
            model.end();
            had_redo && !model.history().can_redo()
        },
        "a new action did not clear the redo stack",
    );

    check(
        "the history is bounded by bytes",
        {
            let mut history = History::new(32 * 1024);
            for seed in 0..8u8 {
                history.record(Bitmap::new(64, 64, [seed, 0, 0, 255]));
            }
            history.bytes() <= history.cap() && history.undo_len() >= 1
        },
        "the history grew past its cap",
    );

    check(
        "PNG round-trips",
        {
            let mut model = Model::new(16, 16);
            model.set_primary(RED);
            model.begin(4, 4, Side::Primary);
            model.end();
            model
                .bitmap()
                .encode_png()
                .ok()
                .and_then(|bytes| Bitmap::decode(&bytes).ok())
                .is_some_and(|decoded| decoded == *model.bitmap())
        },
        "the PNG did not survive a round trip",
    );

    check(
        "extreme coordinates never panic",
        {
            let mut model = Model::new(16, 16);
            model.set_tool(Tool::Line);
            model.begin(i32::MIN, i32::MIN, Side::Primary);
            model.extend(i32::MAX, i32::MAX);
            model.end();
            model.begin(0, 0, Side::Primary);
            model.end();
            model.bitmap().size() == (16, 16)
        },
        "an extreme coordinate panicked or resized the bitmap",
    );

    check(
        "a canvas side is clamped",
        Model::new(u32::MAX, 0).bitmap().size() == (MAX_SIDE, 1),
        "the canvas side was not clamped",
    );

    check(
        "a failed storage never breaks drawing",
        {
            let storage = FailingStorage;
            let mut model = Model::new(16, 16);
            let save_failed = storage.save(b"x").is_err();
            let load_empty = storage.load().is_none();
            model.begin(4, 4, Side::Primary);
            model.end();
            save_failed && load_empty && model.bitmap().get(4, 4) == Some([0, 0, 0, 255])
        },
        "a failing storage affected the model",
    );

    check(
        "memory storage round-trips",
        {
            let storage = MemoryStorage::new();
            storage.save(b"document").ok();
            storage.available() && storage.load().as_deref() == Some(b"document".as_slice())
        },
        "memory storage lost its bytes",
    );

    all
}

/// The number of non-white pixels.
fn lit(bitmap: &Bitmap) -> usize {
    bitmap
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| **pixel != WHITE)
        .count()
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_model_self_test_passes() {
        let mut out = Vec::new();
        assert!(super::run(&mut out), "{}", String::from_utf8_lossy(&out));
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("PASS new canvas"), "{text}");
        assert!(!text.contains("FAIL"), "{text}");
    }
}
