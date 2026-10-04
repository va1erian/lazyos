//! Interactive edge/corner resize: the [`ResizeDrag`] session and
//! its transitions. The window is not re-rendered during the drag; the
//! compositor draws a wireframe outline of the prospective rectangle and
//! applies the new size once, on release.

use user::messenger::display::{wire, Rect};
use user::sys;

use super::compositor::Compositor;
use super::geometry::{self, Edges};
use super::cursor::present_cursor_outside;
use super::theme::{border, title_h};

/// An in-progress interactive resize.
#[derive(Clone, Copy)]
pub(super) struct ResizeDrag {
    /// The surface being resized.
    pub(super) id: u64,
    /// The frame edges the drag grabbed.
    pub(super) edges: Edges,
    /// The window rectangle when the drag began.
    pub(super) start: Rect,
    /// The pointer position when the drag began.
    pub(super) grab: (i32, i32),
    /// The prospective rectangle drawn as the outline (and applied on release).
    pub(super) outline: Rect,
}

impl Compositor {
    /// Begin resizing surface `id` from `edges`, with the pointer at `grab`.
    pub(super) fn begin_resize(&mut self, id: u64, edges: Edges, grab: (i32, i32)) {
        let Some(surface) = self
            .surfaces
            .iter()
            .find(|surface| surface.id == id && !surface.minimized)
        else {
            return;
        };
        let start = surface.window();
        self.resize = Some(ResizeDrag {
            id,
            edges,
            start,
            grab,
            outline: start,
        });
    }

    /// Move the active resize to `new`: compose the old and new outline
    /// rectangles and draw the new wireframe, leaving the window itself at its
    /// old geometry. The cursor overlay moves with the repaint.
    pub(super) fn resize_move(&mut self, new: (i32, i32)) {
        let Some(active) = self.resize else {
            return;
        };
        let Some(hints) = self
            .surfaces
            .iter()
            .find(|surface| surface.id == active.id && !surface.minimized)
            .and_then(|surface| surface.hints)
        else {
            // The surface went away (or is no longer resizable): drop the drag.
            self.resize = None;
            return;
        };
        let rect = geometry::resize_rect(
            active.start,
            active.edges,
            (new.0 - active.grab.0, new.1 - active.grab.1),
            ((hints.min_w, hints.min_h), (hints.max_w, hints.max_h)),
            self.work_area(),
        );
        if let Some(drag) = self.resize.as_mut() {
            drag.outline = rect;
        }
        // The old outline must be erased and the new one drawn; one pixel of
        // slack covers the wireframe's thickness.
        let damage = geometry::inflate(active.outline.union(rect), 1).intersect(self.full());
        let lifted = self.cursor.lift(&mut self.screen);
        self.compose(damage);
        super::anim::outline(&mut self.screen, rect, damage);
        let stamped = self.stamp_cursor();
        let _ = sys::display_present(damage.x, damage.y, damage.w, damage.h);
        present_cursor_outside(lifted, stamped, damage);
    }

    /// Apply the active resize: adopt the outline rectangle, tell the client
    /// and the shell, and repaint. A drag that never moved the frame is a
    /// no-op beyond the repaint that erases the outline.
    pub(super) fn finish_resize(&mut self) {
        let Some(active) = self.resize.take() else {
            return;
        };
        if active.outline != active.start {
            if let Some(surface) = self
                .surfaces
                .iter_mut()
                .find(|surface| surface.id == active.id)
            {
                surface.x = active.outline.x;
                surface.y = active.outline.y;
                surface.w = active.outline.w - border() * 2;
                surface.h = active.outline.h - title_h() - border();
                let (id, events, width, height) =
                    (surface.id, surface.events, surface.w, surface.h);
                self.send_configure(id, events, width, height, wire::WINDOW_STATE_NORMAL);
                self.notify_surface(id, wire::CHANGE_RESIZED);
            }
        }
        self.repaint_full();
    }

    /// Cancel the active resize and erase its outline.
    pub(super) fn cancel_resize(&mut self) {
        if self.resize.take().is_some() {
            self.repaint_full();
        }
    }
}
