//! Cover-crop, resize and circle mask: a picture made exactly the size of
//! the box it is drawn in, so painting is one `draw_image` (R4).

use image::imageops::{self, FilterType};
use image::DynamicImage;

use super::Bitmap;

/// `img` scaled to cover `width` x `height` and centre-cropped to it.
pub fn cover(img: &DynamicImage, width: u32, height: u32) -> Bitmap {
    let (width, height) = (width.max(1), height.max(1));
    let rgba = img.to_rgba8();
    let (sw, sh) = (rgba.width(), rgba.height());
    // The largest source rectangle with the box's aspect ratio, centred.
    let (cw, ch) = if u64::from(sw) * u64::from(height) > u64::from(sh) * u64::from(width) {
        // The source is wider than the box: keep full height.
        let cw = (u64::from(sh) * u64::from(width) / u64::from(height)).max(1) as u32;
        (cw.min(sw), sh)
    } else {
        let ch = (u64::from(sw) * u64::from(height) / u64::from(width)).max(1) as u32;
        (sw, ch.min(sh))
    };
    let (x, y) = ((sw - cw) / 2, (sh - ch) / 2);
    let cropped = imageops::crop_imm(&rgba, x, y, cw, ch).to_image();
    let resized = if (cw, ch) == (width, height) {
        cropped
    } else {
        // Triangle (bilinear) is the speed/quality point for thumbnails.
        imageops::resize(&cropped, width, height, FilterType::Triangle)
    };
    Bitmap {
        width,
        height,
        rgba: resized.into_raw(),
    }
}

/// `img` scaled to fit inside `width` x `height`, whole, keeping its
/// proportions. The result is at least 1 x 1 and never larger than the box.
pub fn fit(img: &DynamicImage, width: u32, height: u32) -> Bitmap {
    let (width, height) = (width.max(1), height.max(1));
    let rgba = img.to_rgba8();
    let (sw, sh) = (rgba.width().max(1), rgba.height().max(1));
    // The scale that makes the picture touch the box on its tighter side.
    let by_width = u64::from(width) * u64::from(sh) <= u64::from(height) * u64::from(sw);
    let (w, h) = if by_width {
        (
            width,
            ((u64::from(width) * u64::from(sh)) / u64::from(sw)).max(1) as u32,
        )
    } else {
        (
            ((u64::from(height) * u64::from(sw)) / u64::from(sh)).max(1) as u32,
            height,
        )
    };
    let resized = if (w, h) == (sw, sh) {
        rgba
    } else {
        imageops::resize(&rgba, w, h, FilterType::CatmullRom)
    };
    Bitmap {
        width: w,
        height: h,
        rgba: resized.into_raw(),
    }
}

/// Makes `bitmap` a circle: alpha falls to 0 outside the inscribed ellipse,
/// with a one-pixel antialiased edge.
pub fn circle_mask(bitmap: &mut Bitmap) {
    let (w, h) = (bitmap.width as f32, bitmap.height as f32);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let (rx, ry) = (w / 2.0, h / 2.0);
    for y in 0..bitmap.height {
        for x in 0..bitmap.width {
            let (dx, dy) = ((x as f32 + 0.5 - cx) / rx, (y as f32 + 0.5 - cy) / ry);
            let distance = (dx * dx + dy * dy).sqrt();
            // `edge` is the distance from the rim in pixels (positive inside).
            let edge = (1.0 - distance) * rx.min(ry);
            let coverage = edge.clamp(0.0, 1.0);
            let at = ((y * bitmap.width + x) * 4 + 3) as usize;
            bitmap.rgba[at] = (f32::from(bitmap.rgba[at]) * coverage).round() as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn two_halves(w: u32, h: u32) -> DynamicImage {
        // Left half red, right half blue.
        DynamicImage::ImageRgba8(RgbaImage::from_fn(w, h, |x, _| {
            if x < w / 2 {
                Rgba([255, 0, 0, 255])
            } else {
                Rgba([0, 0, 255, 255])
            }
        }))
    }

    #[test]
    fn the_result_is_exactly_the_box() {
        for (sw, sh, w, h) in [
            (400, 300, 100, 100),
            (100, 400, 160, 90),
            (50, 50, 200, 120),
            (1, 1, 7, 3),
        ] {
            let b = cover(&two_halves(sw, sh), w, h);
            assert_eq!((b.width, b.height), (w, h));
            assert_eq!(b.rgba.len(), (w * h * 4) as usize);
        }
    }

    #[test]
    fn a_wide_source_is_cropped_at_the_sides_not_squashed() {
        // 400x100 into a square: the centre 100x100 survives, which is the
        // right half of the red and the left half of the blue.
        let b = cover(&two_halves(400, 100), 50, 50);
        let px = |x: u32| &b.rgba[((25 * 50 + x) * 4) as usize..][..3];
        assert_eq!(px(5), [255, 0, 0]);
        assert_eq!(px(45), [0, 0, 255]);
    }

    #[test]
    fn fit_keeps_the_whole_picture_inside_the_box() {
        let wide = two_halves(400, 100);
        let b = fit(&wide, 200, 200);
        assert_eq!((b.width, b.height), (200, 50));
        let tall = two_halves(100, 400);
        let b = fit(&tall, 200, 200);
        assert_eq!((b.width, b.height), (50, 200));
        // It scales up too (a zoom) and never leaves the box.
        let b = fit(&two_halves(10, 10), 300, 100);
        assert_eq!((b.width, b.height), (100, 100));
        // Both halves survive: nothing is cropped.
        let b = fit(&wide, 100, 100);
        assert_eq!(&b.rgba[..3], [255, 0, 0]);
        let last = ((b.height - 1) * b.width + (b.width - 1)) as usize * 4;
        assert_eq!(&b.rgba[last..last + 3], [0, 0, 255]);
    }

    #[test]
    fn a_tall_source_keeps_its_middle() {
        let tall = DynamicImage::ImageRgba8(RgbaImage::from_fn(10, 100, |_, y| {
            if (40..60).contains(&y) {
                Rgba([0, 255, 0, 255])
            } else {
                Rgba([0, 0, 0, 255])
            }
        }));
        let b = cover(&tall, 10, 10);
        // Square crop of the middle 10 rows: rows 45..55, all green.
        assert!(b.rgba.chunks(4).all(|p| p[1] == 255));
    }

    #[test]
    fn the_circle_is_clear_in_the_corners_and_solid_in_the_middle() {
        let mut b = cover(&two_halves(40, 40), 48, 48);
        circle_mask(&mut b);
        let alpha = |x: u32, y: u32| b.rgba[((y * 48 + x) * 4 + 3) as usize];
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(47, 0), 0);
        assert_eq!(alpha(0, 47), 0);
        assert_eq!(alpha(47, 47), 0);
        assert_eq!(alpha(24, 24), 255);
        assert_eq!(alpha(24, 2), 255);
        assert!(alpha(24, 0) > 0 && alpha(24, 0) < 255, "an antialiased rim");
    }
}
