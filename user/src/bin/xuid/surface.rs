//! The composited window model (issue #194 split): the [`Surface`] record and
//! the title-bar [`Drag`] session, split out of `xuid.rs` unchanged.

use alloc::string::String;
use surfbuf::SlotTable;
use user::messenger::display::{wire, Rect};

use super::geometry::SizeHints;
use super::present::Mapping;

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
    /// App pixel buffer mapped into this task (`0` until attached): always
    /// the *current* slot of `slots` (issue #361).
    pub(super) pixels: u64,
    /// Length of the mapped pixel buffer.
    pub(super) bytes: u64,
    /// Attached buffer slots and which one the compositor reads.
    pub(super) slots: SlotTable<Mapping>,
    /// The width of the *current* buffer, which may lag the content size
    /// between a resize and the client's new attach.
    pub(super) buf_w: i32,
    /// The height of the current buffer.
    pub(super) buf_h: i32,
    /// Content-size bounds from `SetSizeHints`; `None` keeps the window
    /// fixed-size (no resize edges, no maximize button).
    pub(super) hints: Option<SizeHints>,
    /// The normal window rectangle saved while maximized; `Some` while the
    /// surface is maximized.
    pub(super) maximized: Option<Rect>,
    /// Hidden by the minimize button; restorable from the taskbar.
    pub(super) minimized: bool,
    /// The bottom-layer desktop surface (issue #167): no chrome, never
    /// focused, hit-tested, minimized, or listed on the taskbar.
    pub(super) desktop: bool,
    /// Keys reach this surface's client through an `inputd` session, so the
    /// legacy `KeyDown`/`KeyUp` synthesis must skip it (`docs/input-plan.md`).
    pub(super) input_session: bool,
}

impl Surface {
    /// The protocol role this surface reports (`wire::ROLE_*`).
    pub(super) fn role(&self) -> u32 {
        if self.desktop {
            wire::ROLE_DESKTOP
        } else {
            wire::ROLE_WINDOW
        }
    }

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

    /// The maximize button, just left of the close button. Only meaningful for
    /// a resizable window; [`Surface::maximize_button`] is still a valid
    /// rectangle for a fixed-size one (where it is simply not drawn).
    pub(super) fn maximize_button(&self) -> Rect {
        let close = self.close_button();
        Rect::new(close.x - BUTTON - BUTTON_GAP, close.y, BUTTON, BUTTON)
    }

    /// The minimize button, just left of the maximize button for a resizable
    /// window and of the close button otherwise.
    pub(super) fn minimize_button(&self) -> Rect {
        let right = if self.resizable() {
            self.maximize_button()
        } else {
            self.close_button()
        };
        Rect::new(right.x - BUTTON - BUTTON_GAP, right.y, BUTTON, BUTTON)
    }

    /// Whether the window may be resized and maximized: it declared size hints
    /// and is not the desktop layer.
    pub(super) fn resizable(&self) -> bool {
        self.hints.is_some() && !self.desktop
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
