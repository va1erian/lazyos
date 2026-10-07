//! Each cell's picture: the fallback chain of `lazyshell::tray::icon`, walked
//! until a picture loads (docs/tray-plan.md section 6.2).
//!
//! Lucide names resolve through the OS-wide named-icon library
//! (`lazyicons`), so an unknown name falls through like any unusable icon.
//! Package PNGs go through the desktop's [`IconCache`] (size cap, PNG header
//! check before decoding). A mask is tinted with the bar's ink here, so it
//! reads on light and dark bars alike. The source a cell ends up with is
//! printed once per change (`SHELL:TRAY:ICON app=<id> source=<...>`), the
//! evidence that a bad icon fell back.

use std::rc::Rc;

use lazyshell::tray::icon::{chain, Picture, FALLBACK_LUCIDE};
use lazyshell::tray::item::Image as ItemImage;
use lazyshell::tray::{Entry, Tray};
use xui_core::image::Image;
use xui_core::Lucide;

use super::super::icons::IconCache;

/// A picture ready to draw.
pub enum Drawn {
    /// An outline in the bar's ink.
    Outline(Lucide),
    /// An image drawn as is (a tinted mask, pixels or a package PNG).
    Image(Rc<Image>),
}

/// Decoded package icons and the source each app last showed.
#[derive(Default)]
pub struct Pictures {
    files: IconCache,
    shown: Vec<(String, String)>,
}

impl Pictures {
    /// The picture of `entry` at UI `scale`, for an app whose package icons
    /// are in `icons_dir`; masks are tinted with `ink` (`0xRRGGBB`).
    pub fn resolve(
        &mut self,
        entry: &Entry,
        icons_dir: Option<&str>,
        scale: i32,
        ink: u32,
    ) -> Drawn {
        let source = entry.custom.as_ref().and_then(|item| item.icon.as_ref());
        let known = |name: &str| lazyicons::from_name(name).is_some();
        for picture in chain(source, scale, icons_dir, known) {
            let (label, drawn) = match picture {
                Picture::Lucide(name) => match lazyicons::from_name(name) {
                    Some(outline) => (format!("lucide:{name}"), Drawn::Outline(outline)),
                    None => continue,
                },
                Picture::Mask(mask) => match tinted(mask, ink) {
                    Some(image) => (String::from("mask"), Drawn::Image(Rc::new(image))),
                    None => continue,
                },
                Picture::Pixels(pixels) => match plain(pixels) {
                    Some(image) => (String::from("pixels"), Drawn::Image(Rc::new(image))),
                    None => continue,
                },
                Picture::File(path) => match self.files.get(&path) {
                    Some(image) => {
                        let name = path.rsplit('/').next().unwrap_or_default();
                        (format!("file:{name}"), Drawn::Image(image))
                    }
                    None => continue,
                },
            };
            self.report(&entry.app, label);
            return drawn;
        }
        // `chain` always ends in the built-in outline, which is in every
        // `lazyicons`; this is only reached if that ever stops being true.
        self.report(&entry.app, format!("lucide:{FALLBACK_LUCIDE}"));
        Drawn::Outline(Lucide::AppWindow)
    }

    /// Forget the apps no longer in the tray, so one that comes back
    /// reports its source again.
    pub fn forget_unused(&mut self, model: &Tray) {
        self.shown.retain(|(app, _)| model.get(app).is_some());
    }

    fn report(&mut self, app: &str, source: String) {
        if let Some(entry) = self.shown.iter_mut().find(|(known, _)| known == app) {
            if entry.1 == source {
                return;
            }
            entry.1.clone_from(&source);
        } else {
            self.shown.push((app.to_owned(), source.clone()));
        }
        println!("SHELL:TRAY:ICON app={app} source={source}");
    }
}

/// `mask`'s alpha with `ink` as the colour.
fn tinted(mask: &ItemImage, ink: u32) -> Option<Image> {
    let [_, r, g, b] = ink.to_be_bytes();
    let pixels = mask
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|pixel| [r, g, b, pixel[3]])
        .collect();
    Image::from_rgba(mask.width, mask.height, pixels).ok()
}

fn plain(pixels: &ItemImage) -> Option<Image> {
    Image::from_rgba(pixels.width, pixels.height, pixels.data.clone()).ok()
}
