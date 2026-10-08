//! LazyShell's models (issue #157): everything the desktop shell decides that
//! does not need a screen, a compositor or a service, so it is tested on the
//! host.
//!
//! * [`taskbar`]: the window list fed by `ListSurfaces`, `SurfaceChanged` and
//!   `FocusChanged`, the bar geometry and the click decision.
//! * [`menu`]: the start-menu rows (installed apps, the configured
//!   `sys/ui/menu` entries, then the power rows with their confirmation) and
//!   their geometry.
//! * [`desktop`]: the desktop icons from the user's desktop folder, their
//!   layout, and the `sys/ui/desktop` launchers that seed the folder.
//! * [`keynav`]: the keyboard rules of the panel menus and the tray (#648).
//! * [`shortcut`]: the `.lnk` shortcut files the desktop folder holds.
//! * [`clock`]: the bar clock text (kernel UTC plus the `timed` zone).
//! * [`notice`]: the "app stopped" notice `init`'s app failures become.
//! * [`policy`]: who may call the `os.lazy.shell` service.
//! * [`wallpaper`]: the desktop picture's size check, crop and brightness.
//! * [`tray`]: the taskbar tray's items, validation, icon fallback and
//!   layout (docs/tray-plan.md).
//!
//! Coordinates are integer pixels; [`Rect`] is `(x, y, w, h)` with an exclusive
//! right/bottom edge.

#![forbid(unsafe_code)]

pub mod clock;
pub mod desktop;
pub mod keynav;
pub mod menu;
pub mod notice;
pub mod policy;
pub mod shortcut;
pub mod taskbar;
pub mod tray;
pub mod wallpaper;

pub use deskmenu::Entry;

/// An axis-aligned rectangle: origin plus size, right/bottom exclusive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    /// A rectangle at `(x, y)` of `w` x `h`.
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }

    /// Whether `(px, py)` lies inside.
    pub const fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }

    /// The same rectangle moved by `(dx, dy)`.
    pub const fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    /// Whether it covers no pixel.
    pub const fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_is_right_and_bottom_exclusive() {
        let rect = Rect::new(4, 2, 80, 28);
        assert!(rect.contains(4, 2));
        assert!(rect.contains(83, 29));
        assert!(!rect.contains(84, 10));
        assert!(!rect.contains(10, 30));
        assert!(!rect.contains(3, 10));
        assert_eq!(rect.offset(0, 736), Rect::new(4, 738, 80, 28));
        assert!(Rect::new(0, 0, 0, 5).is_empty());
    }
}
