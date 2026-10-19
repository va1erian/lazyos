//! `raster.rs` writes what `picture.rs` draws: the same picture through both, compared pixel for pixel, for
//! pictures with patches, shown ahead of the page up and down, with and without bands, over parts of the view.

use std::rc::Rc;

use xui_canvas::Surface;
use xui_core::geometry::Rect;
use xui_core::Image;

use crate::bands::Bands;
use crate::damage::{IRect, Scroll};
use crate::mailbox::{Delta, Patch};
use crate::parts::Parts;
use crate::picture;
use crate::raster::{raster, Pixels};

const W: u32 = 300;
const H: u32 = 200;

/// An opaque image whose every pixel says where it is (and which image it is).
fn pattern(width: u32, height: u32, seed: u8) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&[
                (x % 251) as u8,
                (y % 241) as u8,
                seed.wrapping_add((x / 7 + y / 5) as u8),
                255,
            ]);
        }
    }
    rgba
}

fn picture() -> Parts {
    let mut parts = Parts::default();
    parts.set_full(Image::from_rgba(W, H, pattern(W, H, 1)).unwrap());
    let patch = |x, y, w, h, seed| Patch {
        rect: IRect::new(x, y, w, h),
        rgba: pattern(w as u32, h as u32, seed),
    };
    assert!(parts.apply(Delta {
        size: (W, H),
        scroll: Some(Scroll {
            area: IRect::new(0, 0, W as i32, H as i32),
            dx: 0,
            dy: -30
        }),
        patches: vec![patch(0, 170, 300, 30, 2), patch(40, 50, 60, 40, 3)],
    }));
    parts
}

fn bands(offset: i32) -> Bands<Rc<Image>> {
    let mut bands = Bands::default();
    // Below the view: rows H..H+120 of the page from `offset`, in two bands; above: 50 rows.
    for (top, height, seed) in [
        (offset + H as i32, 70, 9),
        (offset + H as i32 + 70, 50, 10),
        (offset - 50, 50, 11),
    ] {
        bands.insert(
            0,
            top,
            W,
            height,
            Rc::new(Image::from_rgba(W, height as u32, pattern(W, height as u32, seed)).unwrap()),
        );
    }
    bands
}

/// Both paths over a window of 340x260 with the view at (20, 30) and `damage` in window coordinates.
fn both(
    parts: &Parts,
    bands: &Bands<Rc<Image>>,
    ahead: i32,
    offset: i32,
    damage: Rect,
) -> (Vec<u8>, Vec<u8>) {
    let (ww, wh) = (340u32, 260u32);
    let view = Rect::new(20, 30, 20 + W as i32, 30 + H as i32);
    let mut surface = Surface::new(ww, wh);
    surface.with_canvas(view, |canvas| {
        picture::paint(canvas, parts, bands, ahead, offset);
    });
    let canvas = surface.pixels().to_vec();
    let mut direct = canvas.clone();
    // What the direct path does not write must not matter: scribble over the damage first.
    for y in damage.top..damage.bottom {
        for x in damage.left..damage.right {
            let at = ((y as u32 * ww + x as u32) * 4) as usize;
            direct[at..at + 4].copy_from_slice(&[1, 2, 3, 4]);
        }
    }
    let mut pixels = Pixels {
        data: &mut direct,
        stride: ww as usize * 4,
    };
    let ir = |r: Rect| IRect {
        x0: r.left,
        y0: r.top,
        x1: r.right,
        y1: r.bottom,
    };
    raster(
        &mut pixels,
        ir(view),
        ir(damage),
        parts,
        bands,
        ahead,
        offset,
    );
    (canvas, direct)
}

fn assert_same(label: &str, (canvas, direct): (Vec<u8>, Vec<u8>)) {
    if canvas != direct {
        let first = canvas
            .chunks(4)
            .zip(direct.chunks(4))
            .position(|(a, b)| a != b)
            .unwrap();
        panic!(
            "{label}: first difference at pixel {} ({}, {}): canvas {:?} direct {:?}",
            first,
            first % 340,
            first / 340,
            &canvas[first * 4..first * 4 + 4],
            &direct[first * 4..first * 4 + 4]
        );
    }
}

#[test]
fn the_direct_path_writes_what_the_canvas_draws() {
    let parts = picture();
    let whole = Rect::new(20, 30, 320, 230);
    for offset in [0, 37, 1000] {
        let kept = bands(offset);
        for ahead in [0, 15, 64, 199, 260, -10, -64, -120] {
            for (label, damage) in [
                ("whole", whole),
                ("part", Rect::new(60, 100, 250, 210)),
                ("bar", Rect::new(300, 30, 320, 230)),
            ] {
                assert_same(
                    &format!("offset {offset} ahead {ahead} {label} with bands"),
                    both(&parts, &kept, ahead, offset, damage),
                );
                assert_same(
                    &format!("offset {offset} ahead {ahead} {label}"),
                    both(&parts, &Bands::default(), ahead, offset, damage),
                );
            }
        }
    }
}

#[test]
fn a_picture_smaller_than_the_view_is_on_white() {
    let mut parts = Parts::default();
    parts.set_full(Image::from_rgba(200, 120, pattern(200, 120, 4)).unwrap());
    assert_same(
        "small",
        both(&parts, &Bands::default(), 0, 0, Rect::new(20, 30, 320, 230)),
    );
    assert_same(
        "empty",
        both(
            &Parts::default(),
            &Bands::default(),
            0,
            0,
            Rect::new(20, 30, 320, 230),
        ),
    );
}
