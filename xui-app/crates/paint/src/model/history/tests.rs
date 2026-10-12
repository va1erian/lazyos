use super::*;
use crate::model::image::{Layer, LayerProps};
use crate::model::rect::Rect;
use crate::model::{Bitmap, WHITE};

const RED: [u8; 4] = [255, 0, 0, 255];

fn doc() -> Document {
    Document::new(8, 8, WHITE).unwrap()
}

/// Paints `rect` red on `layer` and returns the matching edit.
fn paint(doc: &mut Document, layer: usize, rect: Rect) -> Edit {
    let pixels = &mut doc.layer_mut(layer).unwrap().pixels;
    let before = pixels.crop(rect).unwrap();
    for y in rect.y0..rect.y1 {
        for x in rect.x0..rect.x1 {
            pixels.put(x, y, RED);
        }
    }
    Edit::Pixels {
        name: "Test".into(),
        layer,
        rect,
        before,
    }
}

#[test]
fn undo_and_redo_round_trip_pixels() {
    let mut doc = doc();
    let original = doc.clone();
    let mut history = History::new(DEFAULT_CAP);
    let edit = paint(&mut doc, 0, Rect::new(2, 2, 3, 3));
    let painted = doc.clone();
    history.record(edit);
    assert_eq!(history.undo_name(), Some("Test"));

    let damage = history.undo(&mut doc).unwrap();
    assert_eq!(damage, Damage::Rect(Rect::new(2, 2, 3, 3)));
    assert_eq!(doc, original);
    assert_eq!(history.redo_name(), Some("Test"));
    history.redo(&mut doc).unwrap();
    assert_eq!(doc, painted);
    // And again: the same entry serves both directions.
    history.undo(&mut doc).unwrap();
    assert_eq!(doc, original);
}

#[test]
fn a_new_action_clears_the_redo_stack() {
    let mut doc = doc();
    let mut history = History::new(DEFAULT_CAP);
    let edit = paint(&mut doc, 0, Rect::new(0, 0, 2, 2));
    history.record(edit);
    history.undo(&mut doc).unwrap();
    assert!(history.can_redo());
    let edit = paint(&mut doc, 0, Rect::new(4, 4, 2, 2));
    history.record(edit);
    assert!(!history.can_redo(), "a new action branches from here");
}

#[test]
fn the_cap_evicts_the_oldest_and_keeps_one() {
    let mut doc = Document::new(64, 64, WHITE).unwrap();
    // Each full-layer entry is 16 KiB plus a little; a 40 KiB cap keeps two.
    let mut history = History::new(40 * 1024);
    for _ in 0..8 {
        let edit = paint(&mut doc, 0, Rect::new(0, 0, 64, 64));
        history.record(edit);
    }
    assert!(history.bytes() <= history.cap());
    assert_eq!(history.undo_len(), 2);

    // A cap smaller than one entry still keeps the single newest step.
    let mut tiny = History::new(1);
    let edit = paint(&mut doc, 0, Rect::new(0, 0, 8, 8));
    tiny.record(edit);
    let edit = paint(&mut doc, 0, Rect::new(0, 0, 8, 8));
    tiny.record(edit);
    assert_eq!(tiny.undo_len(), 1);
    assert!(tiny.bytes() > tiny.cap());
}

#[test]
fn small_edits_cost_their_rectangle_not_the_image() {
    let mut doc = Document::new(1024, 1024, WHITE).unwrap();
    let mut history = History::new(4 * 1024 * 1024);
    for i in 0..500 {
        let edit = paint(&mut doc, 0, Rect::new(i, i, 16, 16));
        history.record(edit);
    }
    // 4 MiB of whole-image snapshots held one step; deltas hold hundreds.
    assert_eq!(history.undo_len(), 500);
    assert!(history.bytes() <= history.cap());
}

#[test]
fn layer_commands_round_trip() {
    let mut doc = doc();
    let mut history = History::new(DEFAULT_CAP);
    let before = doc.clone();

    // Add a layer.
    doc.insert_layer(1, Layer::new("Two", Bitmap::transparent(8, 8)));
    doc.set_active(1);
    history.record(Edit::Layer {
        name: "Add Layer".into(),
        command: LayerCommand::Remove {
            index: 1,
            active: 0,
        },
    });
    let added = doc.clone();

    // Change its properties.
    let layer = doc.layer_mut(1).unwrap();
    let old = layer.props();
    layer.opacity = 10;
    layer.name = "Renamed".into();
    history.record(Edit::Layer {
        name: "Layer Properties".into(),
        command: LayerCommand::Props {
            index: 1,
            props: old,
        },
    });
    let renamed = doc.clone();

    // Move it down (the active layer follows).
    doc.move_layer_raw(1, 0);
    doc.set_active(0);
    history.record(Edit::Layer {
        name: "Move Layer".into(),
        command: LayerCommand::Move { from: 0, to: 1 },
    });
    let moved = doc.clone();

    // Delete it.
    let removed = doc.remove_layer(0).unwrap();
    doc.set_active(0);
    history.record(Edit::Layer {
        name: "Delete Layer".into(),
        command: LayerCommand::Insert {
            index: 0,
            layer: removed,
            active: 0,
        },
    });
    let deleted = doc.clone();

    for want in [&moved, &renamed, &added, &before] {
        history.undo(&mut doc).unwrap();
        assert_eq!(&doc, want);
    }
    for want in [&added, &renamed, &moved, &deleted] {
        history.redo(&mut doc).unwrap();
        assert_eq!(&doc, want);
    }
    let _ = LayerProps {
        name: String::new(),
        visible: true,
        opacity: 0,
    };
}

#[test]
fn a_document_edit_swaps_the_whole_document() {
    let mut doc = doc();
    let mut history = History::new(DEFAULT_CAP);
    let bigger = {
        let wide = doc.resized(20, 4);
        std::mem::replace(&mut doc, wide)
    };
    // `bigger` is the old 8x8 document: what undo must bring back.
    history.record(Edit::Document {
        name: "Resize".into(),
        before: Box::new(bigger.clone()),
    });
    assert_eq!(doc.size(), (20, 4));
    assert_eq!(history.undo(&mut doc), Some(Damage::All));
    assert_eq!(doc, bigger);
    history.redo(&mut doc).unwrap();
    assert_eq!(doc.size(), (20, 4));
}
