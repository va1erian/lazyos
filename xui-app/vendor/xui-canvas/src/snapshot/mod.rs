#![forbid(unsafe_code)]

//! Headless screenshots: run an app on the [`OffscreenBackend`] and capture its
//! window as an [`Image`], with no window, display or GPU.
//!
//! ```no_run
//! use xui_canvas::snapshot::{Snapshot, render};
//! use xui_core::Dip;
//! use xui_core::app::{App, Ui};
//!
//! struct MyApp;
//! impl App for MyApp {
//!     type Msg = ();
//!     fn update(&mut self, _: (), _: &mut Ui<()>) {}
//! }
//!
//! let image = render(Snapshot::new(Dip(720.0), Dip(300.0)).dpi(144), |_ui| MyApp)?;
//! image.save_png("window.png")?;
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! The capture happens after the build closure has returned and the runtime
//! has installed the app and drained the messages queued while building. The
//! offscreen backend has no timers, so nothing blinks or animates: two renders
//! of the same app on one machine are byte-identical. Text uses the machine's
//! fonts, so the bytes can differ between machines with different fonts.

use std::fmt;
use std::rc::Rc;

use xui_core::app::{App, Ui};
use xui_core::backend::BackendError;
use xui_core::image::{Image, ImageError};
use xui_core::{Dip, Theme};

use crate::OffscreenBackend;

mod gallery;
mod session;
mod stage;
#[cfg(test)]
mod tests;

pub use gallery::Gallery;
pub use stage::Stage;

/// The largest side, in device pixels, a snapshot may have; a bigger request
/// is refused rather than allocated.
pub const MAX_SIDE_PX: u32 = 8192;

/// The largest DPI a snapshot may use.
const MAX_DPI: u32 = 1024;

/// Why a snapshot could not be taken.
#[derive(Debug)]
pub enum SnapshotError {
    /// The size or DPI is zero, not finite, or larger than [`MAX_SIDE_PX`].
    Size,
    /// The backend or the build closure failed.
    Backend(BackendError),
    /// The window produced no frame.
    NoFrame,
    /// The frame could not be turned into an image.
    Image(ImageError),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::Size => write!(
                f,
                "snapshot size must be at least 1px and at most {MAX_SIDE_PX}px a side, \
                 at a DPI from 1 to {MAX_DPI}"
            ),
            SnapshotError::Backend(error) => write!(f, "snapshot failed: {error}"),
            SnapshotError::NoFrame => f.write_str("the window produced no frame"),
            SnapshotError::Image(error) => write!(f, "snapshot failed: {error}"),
        }
    }
}

impl std::error::Error for SnapshotError {}

impl From<BackendError> for SnapshotError {
    fn from(error: BackendError) -> SnapshotError {
        SnapshotError::Backend(error)
    }
}

impl From<ImageError> for SnapshotError {
    fn from(error: ImageError) -> SnapshotError {
        SnapshotError::Image(error)
    }
}

/// What to render: the window's size in design units, its theme, DPI and title.
#[derive(Clone, Debug)]
pub struct Snapshot {
    width: Dip,
    height: Dip,
    theme: Theme,
    dpi: u32,
    title: String,
}

impl Snapshot {
    /// A light-themed snapshot of a `width` x `height` window at 96 DPI.
    pub fn new(width: Dip, height: Dip) -> Snapshot {
        Snapshot {
            width,
            height,
            theme: Theme::light(),
            dpi: 96,
            title: "snapshot".to_string(),
        }
    }

    /// Renders with `theme`.
    pub fn theme(mut self, theme: Theme) -> Snapshot {
        self.theme = theme;
        self
    }

    /// Renders at `dpi` dots per inch: the image is `dpi / 96` times larger.
    pub fn dpi(mut self, dpi: u32) -> Snapshot {
        self.dpi = dpi;
        self
    }

    /// Sets the window title.
    pub fn title(mut self, title: impl Into<String>) -> Snapshot {
        self.title = title.into();
        self
    }

    /// Checks the size and DPI, so a bad request is an error and never a huge
    /// allocation or an empty surface.
    fn validate(&self) -> Result<(), SnapshotError> {
        if self.dpi == 0 || self.dpi > MAX_DPI {
            return Err(SnapshotError::Size);
        }
        let fits = |dip: Dip| {
            let px = dip.0 * self.dpi as f32 / 96.0;
            px.is_finite() && (1.0..=MAX_SIDE_PX as f32).contains(&px)
        };
        if fits(self.width) && fits(self.height) {
            Ok(())
        } else {
            Err(SnapshotError::Size)
        }
    }
}

/// Runs the app `build` makes and captures its window.
///
/// `build` is the closure you would give `run_app`.
pub fn render<A, F>(snapshot: Snapshot, build: F) -> Result<Image, SnapshotError>
where
    A: App,
    F: FnOnce(&mut Ui<A::Msg>) -> A,
{
    try_render(snapshot, |ui| Ok(build(ui)))
}

/// Like [`render`], for a build closure that can fail (widget constructors
/// return [`BackendError`]): its error comes back as [`SnapshotError::Backend`].
pub fn try_render<A, F>(snapshot: Snapshot, build: F) -> Result<Image, SnapshotError>
where
    A: App,
    F: FnOnce(&mut Ui<A::Msg>) -> Result<A, BackendError>,
{
    render_with(snapshot, build, |_| {})
}

/// Like [`try_render`], and runs `step` after the app is built and before the
/// capture. The [`Stage`] it receives sends messages and injects input, such as
/// a hover or a click, so a snapshot can show a hovered or open state.
///
/// `step` must be `'static` because the backend stores it until the app is
/// ready; share state with it through `Rc`.
pub fn render_with<A, F, S>(snapshot: Snapshot, build: F, step: S) -> Result<Image, SnapshotError>
where
    A: App,
    F: FnOnce(&mut Ui<A::Msg>) -> Result<A, BackendError>,
    S: FnOnce(&Stage<'_, A::Msg>) + 'static,
{
    snapshot.validate()?;
    let backend = Rc::new(OffscreenBackend::with_dpi(snapshot.dpi));
    session::capture_on(&backend, &snapshot, build, step)
}
