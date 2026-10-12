use super::*;
use crate::model::WHITE;

const RED: Pixel = [255, 0, 0, 255];

fn two_layers() -> Document {
    let mut doc = Document::new(4, 4, WHITE).unwrap();
    doc.insert_layer(1, Layer::new("Top", Bitmap::transparent(4, 4)));
    doc
}

#[test]
fn a_single_layer_flattens_to_itself_byte_for_byte() {
    let mut doc = Document::new(3, 3, [10, 20, 30, 0]).unwrap();
    doc.layer_mut(0).unwrap().pixels.put(1, 1, [1, 2, 3, 77]);
    let flat = doc.flatten();
    assert_eq!(flat, doc.active_layer().pixels);
}

#[test]
fn an_opaque_top_pixel_covers_and_a_transparent_one_shows_through() {
    let mut doc = two_layers();
    doc.layer_mut(1).unwrap().pixels.put(1, 1, RED);
    let flat = doc.flatten();
    assert_eq!(flat.get(1, 1), Some(RED));
    assert_eq!(flat.get(0, 0), Some(WHITE));
}

#[test]
fn layer_opacity_and_pixel_alpha_multiply() {
    let mut doc = two_layers();
    doc.layer_mut(1).unwrap().pixels.put(0, 0, RED);
    doc.layer_mut(1).unwrap().pixels.put(1, 0, [255, 0, 0, 128]);
    doc.layer_mut(1).unwrap().opacity = 128;
    let flat = doc.flatten();
    // 255 * 128/255 over white.
    assert_eq!(flat.get(0, 0), Some([255, 127, 127, 255]));
    // 128/255 * 128/255 = 64/255 over white: green falls to 255 - 64.
    assert_eq!(flat.get(1, 0), Some([255, 191, 191, 255]));
}

#[test]
fn hidden_and_zero_opacity_layers_do_not_show() {
    let mut doc = two_layers();
    doc.layer_mut(1).unwrap().pixels.fill(RED);
    doc.layer_mut(1).unwrap().visible = false;
    assert_eq!(doc.flatten().get(2, 2), Some(WHITE));
    doc.layer_mut(1).unwrap().visible = true;
    doc.layer_mut(1).unwrap().opacity = 0;
    assert_eq!(doc.flatten().get(2, 2), Some(WHITE));
    // With every layer hidden the picture is transparent.
    doc.layer_mut(0).unwrap().visible = false;
    assert_eq!(doc.flatten().get(2, 2), Some([0, 0, 0, 0]));
}

#[test]
fn a_dirty_rectangle_updates_only_itself() {
    let mut doc = two_layers();
    let mut cache = doc.flatten();
    doc.layer_mut(1).unwrap().pixels.put(1, 1, RED);
    doc.layer_mut(1).unwrap().pixels.put(3, 3, RED);
    doc.flatten_into(&mut cache, Rect::new(0, 0, 2, 2));
    assert_eq!(cache.get(1, 1), Some(RED));
    assert_eq!(cache.get(3, 3), Some(WHITE), "outside the rectangle");
    doc.flatten_into(&mut cache, Rect::new(-9, -9, 100, 100));
    assert_eq!(cache.get(3, 3), Some(RED), "clipped, not rejected");
    assert_eq!(cache, doc.flatten());
}

#[test]
fn new_documents_respect_the_budget() {
    assert!(check_budget(4096, 8192, 1, DEFAULT_RESERVE).is_ok(), "256 MiB + undo fits");
    let error = Document::new(16384, 16384, WHITE).unwrap_err();
    assert_eq!(error.layers, 1);
    assert!(Document::from_bitmap(Bitmap::white(2, 2)).is_ok());
}

#[test]
fn the_last_layer_cannot_be_removed() {
    let mut doc = Document::new(2, 2, WHITE).unwrap();
    assert!(doc.remove_layer(0).is_none());
    let mut doc = two_layers();
    assert!(doc.remove_layer(1).is_some());
    assert!(doc.remove_layer(0).is_none());
}
