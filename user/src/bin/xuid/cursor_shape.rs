//! Which cursor sprite is showing (docs/cursor-plan.md D5), and a size change.
//!
//! The shape follows the pointer, in this order: a resize in progress shows
//! the grabbed edges' resize cursor, a window or drag-and-drop move the
//! arrow; otherwise the surface under the pointer decides (a panel, then the
//! topmost window: its resize border, else its content's `SetCursor` shape;
//! else the desktop). The title bar and everything else is the arrow.

use alloc::vec::Vec;
use cursorgfx::{Shape, Theme, BUSY_FRAMES};
use user::messenger::display::{Canvas, Rect};
use user::sys;
use user::trace;

use super::compositor::Compositor;
use super::cursor::{present_cursor, CursorOverlay, Look};
use super::geometry;
use super::theme::resize_out;
use super::window::contains;

/// PIT ticks (10 ms) per busy spinner frame.
const BUSY_TICKS: u64 = 10;

impl Compositor {
    /// The look the pointer has now.
    pub(super) fn cursor_look(&self) -> Look {
        let at = self.cursor_target().unwrap_or(self.pointer);
        let shape = self.shape_at(at);
        let frame = if self.cursor.animates(shape) {
            ((sys::clock() / BUSY_TICKS) % u64::from(BUSY_FRAMES)) as u32
        } else {
            0
        };
        Look { shape, frame }
    }

    /// The shape for a pointer at `at`.
    fn shape_at(&self, at: (i32, i32)) -> Shape {
        if let Some(resize) = &self.resize {
            return resize.edges.cursor_shape();
        }
        if self.drag.is_some() || self.drag_session.is_some() {
            return Shape::Default;
        }
        let surfaces = &self.surfaces;
        if let Some(panel) = surfaces
            .iter()
            .rev()
            .find(|surface| surface.is_panel() && contains(surface.window(), at))
        {
            return panel.cursor;
        }
        let window = surfaces.iter().rev().find(|surface| {
            surface.is_window()
                && !surface.minimized
                && contains(geometry::inflate(surface.window(), resize_out()), at)
        });
        match window {
            Some(window) => {
                if window.resizable() && window.maximized.is_none() && self.alt_tab.is_none() {
                    let edges = geometry::hit_edges(window.window(), at);
                    if !edges.is_empty() {
                        return edges.cursor_shape();
                    }
                }
                if contains(window.content(), at) {
                    window.cursor
                } else {
                    Shape::Default
                }
            }
            None => surfaces
                .iter()
                .rev()
                .find(|surface| surface.is_desktop() && contains(surface.window(), at))
                .map_or(Shape::Default, |desktop| desktop.cursor),
        }
    }

    /// Advance the busy spinner: redraw the sprite when its frame moved.
    pub(super) fn tick_cursor(&mut self) {
        if self.cursor.animates(self.cursor.look().shape) {
            self.move_cursor();
        }
    }

    /// Follow `sys/ui/cursor/size`: redraw the sprite at the new size without
    /// a repaint of the scene.
    pub(super) fn apply_cursor_size(&mut self) {
        let Some(size) = self.themefeed.take_cursor_size() else {
            return;
        };
        if size == self.cursor.design_size() {
            return;
        }
        let old = self.cursor.lift(&mut self.screen);
        self.cursor.set_design_size(size);
        let new = match self.cursor_target() {
            Some(at) => self.cursor.stamp(&mut self.screen, at),
            None => Rect::default(),
        };
        present_cursor(old, new);
    }

    /// Follow `ui/cursor/theme`: draw the pointer from the new shapes, or the
    /// built-in ones, without a repaint of the scene. A theme that does not
    /// parse is refused whole and the previous look stays.
    pub(super) fn apply_cursor_theme(&mut self) {
        let Some(text) = self.themefeed.take_cursor_theme() else {
            return;
        };
        let theme = match text.as_deref().map(cursorgfx::format::parse) {
            Some(Ok(theme)) => Some(theme),
            Some(Err(error)) => {
                trace::line!(
                    trace::Channel::Xuid,
                    "CURSOR:THEME:REFUSED line={} {}",
                    error.line,
                    error.what
                );
                return;
            }
            None => None,
        };
        let name = theme
            .as_ref()
            .map_or("builtin", |theme| theme.name.as_str());
        trace::line!(trace::Channel::Xuid, "CURSOR:THEME:APPLIED name={name}");
        let old = self.cursor.lift(&mut self.screen);
        self.cursor.set_theme(theme);
        let new = match self.cursor_target() {
            Some(at) => self.cursor.stamp(&mut self.screen, at),
            None => Rect::default(),
        };
        present_cursor(old, new);
    }
}

/// A one-shape theme for the boot check: a blue triangle arrow.
const TEST_THEME: &str = "lazyos-cursor 1
name selftest
shape default
hot 0 0
layer fill 2060e0 outline 101010
poly 0 0 400 200 200 400
";

/// Boot check of a custom theme on the overlay: its arrow differs from the
/// built-in one, lifting restores the scene exactly, and dropping the theme
/// brings the built-in arrow back byte for byte.
pub(super) fn selftest_theme(
    overlay: &mut CursorOverlay,
    screen: &mut Canvas,
    scene: &[u8],
    read: &dyn Fn(&Canvas) -> Vec<u8>,
) -> Result<(), &'static str> {
    const FAIL: &str = "XUID:CURSOR:FAIL theme
";
    let look = Look {
        shape: Shape::Default,
        frame: 0,
    };
    overlay.set_look(look);
    let mut shot = |overlay: &mut CursorOverlay, theme: Option<Theme>| {
        overlay.set_theme(theme);
        overlay.stamp(screen, (40, 40));
        let image = read(screen);
        overlay.lift(screen);
        image
    };
    let plain = shot(overlay, None);
    let Ok(theme) = cursorgfx::format::parse(TEST_THEME) else {
        return Err(FAIL);
    };
    let custom = shot(overlay, Some(theme));
    let again = shot(overlay, None);
    let restored = read(screen) == scene;
    if custom == plain || again != plain || !restored {
        return Err(FAIL);
    }
    Ok(())
}
