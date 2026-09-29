#![forbid(unsafe_code)]

//! The environment switches behind the example gallery, so a demo can render
//! headlessly and write its screenshots without any code of its own.
//!
//! `XUI_BACKEND=offscreen` makes a demo run on the [`OffscreenBackend`]
//! (`crate::OffscreenBackend`) instead of a window, and `XUI_SNAPSHOT=<dir>`
//! also asks it to save its light and dark screenshots into `<dir>`. With
//! neither set a demo runs exactly as before.

use std::path::{Path, PathBuf};

use xui_core::image::{Image, ImageError};

use super::SnapshotError;

/// What the environment asks of a demo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gallery {
    offscreen: bool,
    dir: Option<PathBuf>,
}

impl Gallery {
    /// Reads `XUI_BACKEND` and `XUI_SNAPSHOT` from the environment.
    pub fn from_env() -> Gallery {
        let backend = std::env::var("XUI_BACKEND").ok();
        let snapshot = std::env::var("XUI_SNAPSHOT").ok();
        Gallery::parse(backend.as_deref(), snapshot.as_deref())
    }

    /// Interprets the two variables' values. An empty `snapshot` counts as
    /// unset; a set `snapshot` implies the offscreen backend, since a demo that
    /// is asked for screenshots never opens a window.
    pub fn parse(backend: Option<&str>, snapshot: Option<&str>) -> Gallery {
        let dir = snapshot.filter(|dir| !dir.is_empty()).map(PathBuf::from);
        Gallery {
            offscreen: backend == Some("offscreen") || dir.is_some(),
            dir,
        }
    }

    /// Whether the demo should run on the offscreen backend.
    pub fn offscreen(&self) -> bool {
        self.offscreen
    }

    /// The directory screenshots go to, when they were asked for.
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Saves `image` as `<dir>/<name>-<variant>.png`, creating the directory,
    /// and returns the path. Fails when no directory was asked for.
    pub fn save(&self, name: &str, variant: &str, image: &Image) -> Result<PathBuf, SnapshotError> {
        let io = |message: String| SnapshotError::Image(ImageError::Io(message));
        let dir = self
            .dir()
            .ok_or_else(|| io("XUI_SNAPSHOT is not set".into()))?;
        std::fs::create_dir_all(dir).map_err(|error| io(error.to_string()))?;
        let path = dir.join(format!("{name}-{variant}.png"));
        image.save_png(&path)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_variables_change_nothing() {
        let gallery = Gallery::parse(None, None);
        assert!(!gallery.offscreen());
        assert_eq!(gallery.dir(), None);
    }

    #[test]
    fn other_backends_and_an_empty_snapshot_change_nothing() {
        for backend in ["canvas", "native", "", "Offscreen"] {
            let gallery = Gallery::parse(Some(backend), Some(""));
            assert!(!gallery.offscreen(), "{backend:?}");
            assert_eq!(gallery.dir(), None);
        }
    }

    #[test]
    fn offscreen_alone_renders_without_saving() {
        let gallery = Gallery::parse(Some("offscreen"), None);
        assert!(gallery.offscreen());
        assert_eq!(gallery.dir(), None);
    }

    #[test]
    fn a_snapshot_directory_implies_offscreen() {
        let gallery = Gallery::parse(Some("canvas"), Some("out"));
        assert!(gallery.offscreen());
        assert_eq!(gallery.dir(), Some(Path::new("out")));
    }

    #[test]
    fn save_writes_named_files_and_needs_a_directory() {
        let image = Image::from_rgba(1, 1, vec![1, 2, 3, 255]).unwrap();
        let dir = std::env::temp_dir()
            .join(format!("xui-gallery-{}", std::process::id()))
            .join("nested");
        let gallery = Gallery::parse(None, dir.to_str());
        let path = gallery.save("demo", "dark", &image).unwrap();
        assert_eq!(path, dir.join("demo-dark.png"));
        assert!(path.is_file());
        assert!(Gallery::parse(None, None).save("d", "x", &image).is_err());
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }
}
