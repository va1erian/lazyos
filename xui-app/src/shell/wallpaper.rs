//! The desktop picture: the PNG or JPEG file `sys/ui/wallpaper` names, cut
//! and scaled once to cover the screen, then handed to the backend as the
//! desktop window's backdrop.
//!
//! The path is a setting, so the file is untrusted: it is read with a size
//! cap and the size its header declares is checked before the decoder
//! allocates anything. A missing or bad picture leaves the plain background
//! colour and is logged once per path.
//!
//! Serial markers: `SHELL:WALLPAPER:PASS path=<path> size=<w>x<h>` (the
//! picture's own size), `SHELL:WALLPAPER:FAIL path=<path> <why>`, and
//! `SHELL:WALLPAPER:NONE` when the setting is cleared.

use std::io::Read;
use std::rc::Rc;

use lazyshell::wallpaper::{cover, dimensions, mean_rgb};
use lazyshell::Rect;
use xui_core::image::Image;

/// Most bytes read from a picture file.
const MAX_BYTES: u64 = 32 * 1024 * 1024;
/// Most pixels a picture may declare (a 5K screen's worth; 64 MiB decoded).
const MAX_PIXELS: u64 = 16 * 1024 * 1024;
/// The brightness of the launcher column is sampled every this many pixels.
const SAMPLE_STEP: usize = 8;

/// The picture in effect.
#[derive(Default)]
pub struct Wallpaper {
    /// The path last tried (empty: none), so each is loaded once.
    path: String,
    image: Option<Rc<Image>>,
    /// Whether the picture is dark under the launchers (`None`: no picture).
    dark: Option<bool>,
}

impl Wallpaper {
    /// Follow the setting: load `path` (empty: no picture) fitted to a
    /// `screen` of physical pixels, judging its brightness over `labels`.
    /// `true` when the picture changed.
    pub fn sync(&mut self, path: &str, screen: (i32, i32), labels: Rect) -> bool {
        if path == self.path {
            return false;
        }
        let had = self.image.is_some();
        self.path = path.to_owned();
        self.image = None;
        self.dark = None;
        if path.is_empty() {
            println!("SHELL:WALLPAPER:NONE");
            return had;
        }
        match load(path, screen) {
            Ok((image, size)) => {
                println!(
                    "SHELL:WALLPAPER:PASS path={path} size={}x{}",
                    size.0, size.1
                );
                self.dark = mean_rgb(image.pixels(), image.width() as usize, labels, SAMPLE_STEP)
                    .map(|mean| uitheme::text_on(mean) == uitheme::LIGHT_TEXT);
                self.image = Some(Rc::new(image));
                true
            }
            Err(why) => {
                println!("SHELL:WALLPAPER:FAIL path={path} {why}");
                had
            }
        }
    }

    /// The fitted picture, or `None` for the plain background.
    pub fn image(&self) -> Option<Rc<Image>> {
        self.image.clone()
    }

    /// Whether light text reads better on the picture than dark text.
    pub fn dark(&self) -> Option<bool> {
        self.dark
    }
}

/// Read, check and decode `path`, then fit it to `screen`; also the size of
/// the picture as stored.
fn load(path: &str, screen: (i32, i32)) -> Result<(Image, (u32, u32)), String> {
    let file = std::fs::File::open(path).map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(String::from("file too large"));
    }
    let size = dimensions(&bytes).ok_or("not a PNG or JPEG picture")?;
    if u64::from(size.0) * u64::from(size.1) > MAX_PIXELS {
        return Err(format!("{}x{} is too large", size.0, size.1));
    }
    let image = Image::decode(&bytes).map_err(|error| error.to_string())?;
    drop(bytes);
    let target = (screen.0.max(1) as u32, screen.1.max(1) as u32);
    Ok((fit(&image, target)?, size))
}

/// `image` cut to `target`'s aspect ratio and scaled to exactly `target`, so
/// painting it is a plain copy.
fn fit(image: &Image, target: (u32, u32)) -> Result<Image, String> {
    let crop = cover(image.size(), target).ok_or("empty picture")?;
    let whole = (crop.w as u32, crop.h as u32) == image.size();
    let cut = if whole {
        image.clone()
    } else {
        cropped(image, crop)?
    };
    cut.resized(target.0, target.1)
        .map_err(|error| error.to_string())
}

/// The `crop` part of `image` (which [`cover`] keeps inside it).
fn cropped(image: &Image, crop: Rect) -> Result<Image, String> {
    let stride = image.width() as usize * 4;
    let (left, width) = (crop.x as usize * 4, crop.w as usize * 4);
    let mut pixels = Vec::with_capacity(width * crop.h as usize);
    for row in image
        .pixels()
        .chunks_exact(stride)
        .skip(crop.y as usize)
        .take(crop.h as usize)
    {
        pixels.extend_from_slice(&row[left..left + width]);
    }
    Image::from_rgba(crop.w as u32, crop.h as u32, pixels).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `width` x `height` picture whose pixel `(x, y)` is `(x, y, 0)`.
    fn ramp(width: u32, height: u32) -> Image {
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[x as u8, y as u8, 0, 255]);
            }
        }
        Image::from_rgba(width, height, pixels).expect("image")
    }

    #[test]
    fn fit_crops_the_centre_and_reaches_the_target_size() {
        // 8x4 onto a square: the middle four columns, unscaled.
        let fitted = fit(&ramp(8, 4), (4, 4)).expect("fit");
        assert_eq!(fitted.size(), (4, 4));
        assert_eq!(fitted.pixel(0, 0), Some([2, 0, 0, 255]));
        assert_eq!(fitted.pixel(3, 3), Some([5, 3, 0, 255]));
        // The same shape is only scaled.
        assert_eq!(fit(&ramp(8, 4), (16, 8)).expect("fit").size(), (16, 8));
    }

    #[test]
    fn a_missing_or_bad_picture_leaves_none_and_is_tried_once() {
        let mut wallpaper = Wallpaper::default();
        let labels = Rect::new(0, 0, 4, 4);
        assert!(
            !wallpaper.sync("", (8, 8), labels),
            "nothing before, nothing now"
        );
        assert!(!wallpaper.sync("/no/such/picture.jpg", (8, 8), labels));
        assert!(wallpaper.image().is_none() && wallpaper.dark().is_none());
        assert!(!wallpaper.sync("/no/such/picture.jpg", (8, 8), labels));
    }

    #[test]
    fn a_picture_file_loads_fitted_and_reports_its_brightness() {
        let path = std::env::temp_dir().join(format!("lazyshell-wp-{}.png", std::process::id()));
        ramp(8, 4).save_png(&path).expect("save");
        let name = path.to_str().expect("utf-8 path");
        let mut wallpaper = Wallpaper::default();
        assert!(wallpaper.sync(name, (4, 4), Rect::new(0, 0, 4, 4)));
        assert_eq!(wallpaper.image().expect("image").size(), (4, 4));
        assert_eq!(wallpaper.dark(), Some(true), "a near-black ramp");
        assert!(
            !wallpaper.sync(name, (4, 4), Rect::new(0, 0, 4, 4)),
            "same path"
        );
        assert!(wallpaper.sync("", (4, 4), Rect::new(0, 0, 4, 4)), "cleared");
        assert!(wallpaper.image().is_none());
        let _ = std::fs::remove_file(&path);
    }
}
