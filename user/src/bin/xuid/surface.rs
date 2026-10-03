//! The composited window model (issue #194 split): the [`Surface`] record and
//! the title-bar [`Drag`] session, split out of `xuid.rs` unchanged.

use alloc::string::String;
use surfbuf::SlotTable;
use user::messenger::display::{wire, Rect};

use super::geometry::SizeHints;
use super::present::Mapping;

use super::theme::{border, button, button_gap, button_margin, title_h};

/// One composited window.
pub(super) struct Surface {
    /// Protocol id.
    pub(super) id: u64,
    /// Window title from `CreateSurface`.
    pub(super) title: String,
    /// Window top-left (content origin is `(x + border(), y + title_h())`).
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
    /// Hidden by the minimize button; restored by the shell or Alt+Tab.
    pub(super) minimized: bool,
    /// What the surface is (`wire::ROLE_*`): a decorated window, the
    /// bottom-layer desktop (issue #167) or a shell panel painted above every
    /// window (issue #157). Desktops and panels have no chrome and are never
    /// focused, minimized or listed in Alt+Tab.
    pub(super) role: u32,
    /// Where the shell's taskbar entry for this window is (`SetIconGeometry`),
    /// so the minimize/restore zoom flies to it.
    pub(super) icon: Option<Rect>,
    /// Keys reach this surface's client through an `inputd` session, so the
    /// legacy `KeyDown`/`KeyUp` synthesis must skip it (`docs/input-plan.md`).
    pub(super) input_session: bool,
}

impl Surface {
    /// Whether this is a decorated application window.
    pub(super) fn is_window(&self) -> bool {
        self.role == wire::ROLE_WINDOW
    }

    /// Whether this is the bottom-layer desktop.
    pub(super) fn is_desktop(&self) -> bool {
        self.role == wire::ROLE_DESKTOP
    }

    /// Whether this is a shell panel (taskbar, start menu).
    pub(super) fn is_panel(&self) -> bool {
        self.role == wire::ROLE_PANEL
    }

    /// The whole surface rectangle: the decorated window, or for a chromeless
    /// desktop or panel just its pixels.
    pub(super) fn window(&self) -> Rect {
        if !self.is_window() {
            return Rect::new(self.x, self.y, self.w, self.h);
        }
        Rect::new(
            self.x,
            self.y,
            self.w + border() * 2,
            self.h + title_h() + border(),
        )
    }

    /// The title-bar rectangle.
    pub(super) fn title_bar(&self) -> Rect {
        Rect::new(self.x, self.y, self.w + border() * 2, title_h())
    }

    /// The content (app pixel) rectangle; client coordinates are relative to
    /// its origin. A chromeless surface's content is the whole surface.
    pub(super) fn content(&self) -> Rect {
        if !self.is_window() {
            return Rect::new(self.x, self.y, self.w, self.h);
        }
        Rect::new(self.x + border(), self.y + title_h(), self.w, self.h)
    }

    /// The close button, inset in the title bar's right end.
    pub(super) fn close_button(&self) -> Rect {
        Rect::new(
            self.x + self.w + border() * 2 - button_margin() - button(),
            self.y + (title_h() - button()) / 2,
            button(),
            button(),
        )
    }

    /// The maximize button, just left of the close button. Only meaningful for
    /// a resizable window; [`Surface::maximize_button`] is still a valid
    /// rectangle for a fixed-size one (where it is simply not drawn).
    pub(super) fn maximize_button(&self) -> Rect {
        let close = self.close_button();
        Rect::new(
            close.x - button() - button_gap(),
            close.y,
            button(),
            button(),
        )
    }

    /// The minimize button, just left of the maximize button for a resizable
    /// window and of the close button otherwise.
    pub(super) fn minimize_button(&self) -> Rect {
        let right = if self.resizable() {
            self.maximize_button()
        } else {
            self.close_button()
        };
        Rect::new(
            right.x - button() - button_gap(),
            right.y,
            button(),
            button(),
        )
    }

    /// Whether the window may be resized and maximized: it declared size hints
    /// and is a window (not the desktop or a panel).
    pub(super) fn resizable(&self) -> bool {
        self.hints.is_some() && self.is_window()
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
