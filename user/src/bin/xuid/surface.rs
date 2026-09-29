//! The composited window model (issue #194 split): the [`Surface`] record and
//! the title-bar [`Drag`] session, split out of `xuid.rs` unchanged.

use alloc::string::String;
use user::messenger::display::Rect;

use super::theme::{BORDER, BUTTON, BUTTON_GAP, BUTTON_MARGIN, TITLE_H};

/// One composited window.
pub(super) struct Surface {
    /// Protocol id.
    pub(super) id: u64,
    /// Window title from `CreateSurface`.
    pub(super) title: String,
    /// Window top-left (content origin is `(x + BORDER, y + TITLE_H)`).
    pub(super) x: i32,
    pub(super) y: i32,
    /// Content size in pixels.
    pub(super) w: i32,
    pub(super) h: i32,
    /// Event endpoint handle in this task's table.
    pub(super) events: u64,
    /// Task slot that created the surface; only it may start or cancel a drag
    /// for this surface (issue #145).
    pub(super) owner: u64,
    /// App pixel buffer mapped into this task (`0` until attached).
    pub(super) pixels: u64,
    /// Length of the mapped pixel buffer.
    pub(super) bytes: u64,
    /// Hidden by the minimize button; restorable from the taskbar.
    pub(super) minimized: bool,
    /// The bottom-layer desktop surface (issue #167): no chrome, never
    /// focused, hit-tested, minimized, or listed on the taskbar.
    pub(super) desktop: bool,
}

impl Surface {
    /// The whole decorated window rectangle.
    pub(super) fn window(&self) -> Rect {
        Rect::new(
            self.x,
            self.y,
            self.w + BORDER * 2,
            self.h + TITLE_H + BORDER,
        )
    }

    /// The title-bar rectangle.
    pub(super) fn title_bar(&self) -> Rect {
        Rect::new(self.x, self.y, self.w + BORDER * 2, TITLE_H)
    }

    /// The content (app pixel) rectangle.
    pub(super) fn content(&self) -> Rect {
        Rect::new(self.x + BORDER, self.y + TITLE_H, self.w, self.h)
    }

    /// The close button, inset in the title bar's right end.
    pub(super) fn close_button(&self) -> Rect {
        Rect::new(
            self.x + self.w + BORDER * 2 - BUTTON_MARGIN - BUTTON,
            self.y + (TITLE_H - BUTTON) / 2,
            BUTTON,
            BUTTON,
        )
    }

    /// The minimize button, just left of the close button.
    pub(super) fn minimize_button(&self) -> Rect {
        let close = self.close_button();
        Rect::new(close.x - BUTTON - BUTTON_GAP, close.y, BUTTON, BUTTON)
    }
}

/// An in-progress title-bar drag.
#[derive(Clone, Copy)]
pub(super) struct Drag {
    /// The surface being moved.
    pub(super) id: u64,
    /// Pointer offset from the window origin at grab time.
    pub(super) grab_x: i32,
    /// Pointer offset from the window origin at grab time.
    pub(super) grab_y: i32,
}
