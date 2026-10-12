use super::*;
use crate::model::Rect;

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

fn zoomed(zoom: f64, doc: (u32, u32), view: (i32, i32)) -> Viewport {
    let mut vp = Viewport::new();
    vp.zoom_to(zoom, (0, 0), doc, view);
    vp
}

fn at(image: &ScreenImage, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * image.width + x) * 4) as usize;
    [image.rgba[i], image.rgba[i + 1], image.rgba[i + 2], image.rgba[i + 3]]
}

#[test]
fn actual_size_copies_pixels() {
    let mut flat = Bitmap::white(4, 3);
    flat.put(2, 1, RED);
    let image = render(&flat, &Viewport::new(), (100, 100), true).unwrap();
    assert_eq!((image.width, image.height), (4, 3), "only the picture");
    assert_eq!(at(&image, 2, 1), RED);
    assert_eq!(at(&image, 0, 0), [255, 255, 255, 255]);
}

#[test]
fn zoom_in_is_nearest_neighbour_not_blurred() {
    let mut flat = Bitmap::white(4, 4);
    flat.put(1, 1, RED);
    flat.put(2, 1, BLUE);
    let image = render(&flat, &zoomed(4.0, (4, 4), (100, 100)), (100, 100), false).unwrap();
    assert_eq!((image.width, image.height), (16, 16));
    for (x, y) in [(4, 4), (7, 7), (5, 6)] {
        assert_eq!(at(&image, x, y), RED, "({x}, {y})");
    }
    for (x, y) in [(8, 4), (11, 7)] {
        assert_eq!(at(&image, x, y), BLUE, "({x}, {y})");
    }
    assert_eq!(at(&image, 3, 4), [255, 255, 255, 255], "no bleed at the edge");
}

#[test]
fn zoom_out_samples_the_picture() {
    let mut flat = Bitmap::white(8, 8);
    flat.put(4, 4, RED);
    let vp = zoomed(0.5, (8, 8), (100, 100));
    let image = render(&flat, &vp, (100, 100), false).unwrap();
    assert_eq!((image.width, image.height), (4, 4));
    assert_eq!(at(&image, 2, 2), RED);
}

#[test]
fn transparent_pixels_show_a_checkerboard() {
    let mut flat = Bitmap::transparent(32, 32);
    flat.put(0, 0, RED);
    let image = render(&flat, &Viewport::new(), (100, 100), false).unwrap();
    assert_eq!(at(&image, 0, 0), RED);
    assert_eq!(at(&image, 9, 1), [DARK, DARK, DARK, 255], "cell (1,0) is dark");
    assert_eq!(at(&image, 1, 1), [LIGHT, LIGHT, LIGHT, 255]);
}

#[test]
fn half_transparent_blends_with_the_checker() {
    let flat = Bitmap::new(16, 16, [0, 0, 0, 128]);
    let image = render(&flat, &Viewport::new(), (16, 16), false).unwrap();
    let light = at(&image, 0, 0)[0];
    let dark = at(&image, 8, 0)[0];
    assert!(light > dark && light < 160, "{light} {dark}");
}

#[test]
fn the_grid_appears_from_800_percent() {
    let flat = Bitmap::white(8, 8);
    let low = render(&flat, &zoomed(4.0, (8, 8), (64, 64)), (64, 64), true).unwrap();
    assert_eq!(at(&low, 4, 4), [255, 255, 255, 255], "no grid at 400%");
    let high = render(&flat, &zoomed(8.0, (8, 8), (64, 64)), (64, 64), true).unwrap();
    assert_ne!(at(&high, 8, 3), [255, 255, 255, 255], "a vertical line");
    assert_ne!(at(&high, 3, 8), [255, 255, 255, 255], "a horizontal line");
    assert_eq!(at(&high, 3, 3), [255, 255, 255, 255], "cells stay clean");
    let off = render(&flat, &zoomed(8.0, (8, 8), (64, 64)), (64, 64), false).unwrap();
    assert_eq!(at(&off, 8, 3), [255, 255, 255, 255]);
}

#[test]
fn only_the_visible_window_is_built() {
    let flat = Bitmap::white(4096, 4096);
    let image = render(&flat, &Viewport::new(), (640, 480), false).unwrap();
    assert_eq!((image.width, image.height), (640, 480));
    assert!(render(&flat, &Viewport::new(), (0, 0), false).is_none());
}

#[test]
fn the_cache_follows_dirty_rectangles() {
    let mut doc = Document::new(8, 8, [255, 255, 255, 255]).unwrap();
    let mut cache = FlatCache::new();
    assert_eq!(cache.update(&doc, Damage::All), &doc.flatten());
    doc.layer_mut(0).unwrap().pixels.put(1, 1, RED);
    doc.layer_mut(0).unwrap().pixels.put(6, 6, RED);
    cache.update(&doc, Damage::Rect(Rect::new(0, 0, 3, 3)));
    assert_eq!(cache.get().unwrap().get(1, 1), Some(RED));
    assert_eq!(cache.get().unwrap().get(6, 6), Some([255, 255, 255, 255]));
    cache.update(&doc, Damage::None);
    assert_eq!(cache.get().unwrap().get(6, 6), Some([255, 255, 255, 255]));
    cache.update(&doc, Damage::All);
    assert_eq!(cache.get(), Some(&doc.flatten()));
    // A new size rebuilds even without `All`.
    let bigger = Document::new(9, 9, [255, 255, 255, 255]).unwrap();
    assert_eq!(cache.update(&bigger, Damage::None).size(), (9, 9));
}
