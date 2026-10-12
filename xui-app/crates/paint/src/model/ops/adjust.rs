#![forbid(unsafe_code)]

//! Colour adjustments: pure functions over `(&mut Bitmap, rect)`.
//!
//! Each adjustment is a per-pixel kernel run by [`map_rect`], the one place
//! that walks pixels. A selection mask (P2) becomes one more argument of that
//! walker (blend the kernel's result with the old pixel by the mask), and no
//! adjustment changes. A fully transparent pixel is left alone: its colour is
//! invisible, and leaving it keeps an erased area bit for bit what it was.

use crate::model::bitmap::{Bitmap, Pixel};
use crate::model::rect::Rect;

/// Runs `kernel` over every pixel of `rect` (clipped to the bitmap) with
/// alpha above 0. The kernel returns the new pixel; the adjustments below
/// all keep the alpha they were given.
pub fn map_rect(bitmap: &mut Bitmap, rect: Rect, kernel: impl Fn(Pixel) -> Pixel) {
    let rect = rect.intersect(bitmap.rect());
    if rect.is_empty() {
        return;
    }
    let width = rect.width() as usize;
    let stride = bitmap.width() as usize * 4;
    for y in rect.y0..rect.y1 {
        let at = y as usize * stride + rect.x0 as usize * 4;
        let row = &mut bitmap.pixels_mut()[at..at + width * 4];
        for pixel in row.as_chunks_mut::<4>().0 {
            if pixel[3] != 0 {
                *pixel = kernel(*pixel);
            }
        }
    }
}

/// Inverts the colour channels; alpha is kept.
pub fn invert(bitmap: &mut Bitmap, rect: Rect) {
    map_rect(bitmap, rect, |p| [255 - p[0], 255 - p[1], 255 - p[2], p[3]]);
}

/// Rec. 601 luma of a colour, rounded: `(0.299 r + 0.587 g + 0.114 b)` in
/// 16-bit fixed point (the weights sum to exactly 65536, so grey is kept).
fn luma(p: Pixel) -> u8 {
    let sum = 19595 * u32::from(p[0]) + 38470 * u32::from(p[1]) + 7471 * u32::from(p[2]);
    ((sum + 32768) >> 16) as u8
}

/// Desaturates to the luma grey; alpha is kept.
pub fn black_and_white(bitmap: &mut Bitmap, rect: Rect) {
    map_rect(bitmap, rect, |p| {
        let y = luma(p);
        [y, y, y, p[3]]
    });
}

/// The classic sepia matrix in 8-bit fixed point (rows sum to 346/308/241 of
/// 256, so white ends up a warm cream, not white); alpha is kept.
pub fn sepia(bitmap: &mut Bitmap, rect: Rect) {
    map_rect(bitmap, rect, |p| {
        let (r, g, b) = (u32::from(p[0]), u32::from(p[1]), u32::from(p[2]));
        let tone =
            |wr: u32, wg: u32, wb: u32| ((wr * r + wg * g + wb * b + 128) >> 8).min(255) as u8;
        [
            tone(101, 197, 48),
            tone(89, 176, 43),
            tone(70, 137, 34),
            p[3],
        ]
    });
}

/// Brightness and contrast, each -100..=100 (clamped), Paint.NET style.
///
/// The correction depends on the pixel's luma `i` and is the same shift on
/// all three channels, so hue is kept:
/// `shift = (i - 127 + brightness) * m / d + 127 - i`, where `m / d` is
/// `(contrast + 100) / 100` for a negative contrast and `100 / (100 -
/// contrast)` for a positive one. At contrast 100 the picture is thresholded
/// to black and white at luma `128 - brightness`.
pub fn brightness_contrast(bitmap: &mut Bitmap, rect: Rect, brightness: i32, contrast: i32) {
    let brightness = brightness.clamp(-100, 100);
    let contrast = contrast.clamp(-100, 100);
    if brightness == 0 && contrast == 0 {
        return;
    }
    let (multiply, divide) = match contrast {
        c if c < 0 => (c + 100, 100),
        c if c > 0 => (100, 100 - c),
        _ => (1, 1),
    };
    map_rect(bitmap, rect, |p| {
        let i = i32::from(luma(p));
        if divide == 0 {
            let v = if i + brightness < 128 { 0 } else { 255 };
            return [v, v, v, p[3]];
        }
        let shift = (i - 127 + brightness) * multiply / divide + 127 - i;
        let c = |v: u8| (i32::from(v) + shift).clamp(0, 255) as u8;
        [c(p[0]), c(p[1]), c(p[2]), p[3]]
    });
}

/// Hue (-180..=180 degrees), saturation (0..=200 percent, 100 unchanged) and
/// lightness (-100..=100, towards black or white) in HSL. All three at their
/// neutral values (0, 100, 0) is a no-op.
pub fn hue_saturation(bitmap: &mut Bitmap, rect: Rect, hue: i32, saturation: i32, lightness: i32) {
    let hue = hue.clamp(-180, 180);
    let saturation = saturation.clamp(0, 200);
    let lightness = lightness.clamp(-100, 100);
    if hue == 0 && saturation == 100 && lightness == 0 {
        return;
    }
    let (turn, sat, light) = (
        hue as f32 / 360.0,
        saturation as f32 / 100.0,
        lightness as f32 / 100.0,
    );
    map_rect(bitmap, rect, |p| {
        let (mut h, mut s, mut l) = to_hsl(p);
        h = (h + turn).rem_euclid(1.0);
        s = (s * sat).clamp(0.0, 1.0);
        l = if light >= 0.0 {
            l + (1.0 - l) * light
        } else {
            l * (1.0 + light)
        };
        let [r, g, b] = from_hsl(h, s, l.clamp(0.0, 1.0));
        [r, g, b, p[3]]
    });
}

/// Hue (0..1), saturation and lightness (0..1) of a pixel's colour.
fn to_hsl(p: Pixel) -> (f32, f32, f32) {
    let (r, g, b) = (
        f32::from(p[0]) / 255.0,
        f32::from(p[1]) / 255.0,
        f32::from(p[2]) / 255.0,
    );
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    let d = max - min;
    if d == 0.0 {
        return (0.0, 0.0, l);
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

/// The inverse of [`to_hsl`], rounded to 8 bits.
fn from_hsl(h: f32, s: f32, l: f32) -> [u8; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h * 6.0).rem_euclid(2.0) - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match (h * 6.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let byte = |v: f32| ((v + m) * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    [byte(r), byte(g), byte(b)]
}

/// An adjustment the Adjustments menu offers, with its parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adjustment {
    /// Invert Colors.
    Invert,
    /// Black and White.
    BlackAndWhite,
    /// Sepia.
    Sepia,
    /// Brightness/Contrast, each -100..=100.
    BrightnessContrast {
        /// Added to every channel (scaled by contrast), -100..=100.
        brightness: i32,
        /// Spread around the middle, -100..=100.
        contrast: i32,
    },
    /// Hue/Saturation.
    HueSaturation {
        /// Degrees, -180..=180.
        hue: i32,
        /// Percent, 0..=200 (100 unchanged).
        saturation: i32,
        /// -100..=100.
        lightness: i32,
    },
}

impl Adjustment {
    /// The History / Edit menu name.
    pub fn name(&self) -> &'static str {
        match self {
            Adjustment::Invert => "Invert Colors",
            Adjustment::BlackAndWhite => "Black and White",
            Adjustment::Sepia => "Sepia",
            Adjustment::BrightnessContrast { .. } => "Brightness/Contrast",
            Adjustment::HueSaturation { .. } => "Hue/Saturation",
        }
    }

    /// Applies it to `rect` of `bitmap`.
    pub fn apply(&self, bitmap: &mut Bitmap, rect: Rect) {
        match *self {
            Adjustment::Invert => invert(bitmap, rect),
            Adjustment::BlackAndWhite => black_and_white(bitmap, rect),
            Adjustment::Sepia => sepia(bitmap, rect),
            Adjustment::BrightnessContrast {
                brightness,
                contrast,
            } => brightness_contrast(bitmap, rect, brightness, contrast),
            Adjustment::HueSaturation {
                hue,
                saturation,
                lightness,
            } => hue_saturation(bitmap, rect, hue, saturation, lightness),
        }
    }
}
