//! A new window's open zoom, run one frame at a time from the main loop.
//!
//! The other animations ([`Compositor::zoom`]) block the compositor, which is
//! fine for a minimize or a maximize: the window's owner is not waiting for
//! anything. A window that opens is different: its app has just asked for the
//! surface and cannot attach a buffer, present its first frame or open its
//! input session until the compositor answers. Animating inside
//! `CreateSurface` held every app back by the whole zoom (about a quarter of
//! a second), then showed the placeholder spinner while it built and painted.
//!
//! So `CreateSurface` answers at once and starts an [`Opening`]: the window
//! stays hidden while its wireframe flies in, the main loop draws one frame
//! per pass ([`Compositor::tick_opening`], parking at most a tick while one
//! is in flight) and serves the app's requests in between. By the time the
//! zoom lands the app has usually presented, so the window appears with its
//! content and the spinner is never seen.
//!
//! The window is hidden (minimized) while it flies in, so anything that
//! reads or changes window state (minimize, maximize, restore, the owner's
//! `RequestSize`, placing another new window) first lands the zoom in flight
//! ([`Compositor::finish_opening`], [`Compositor::finish_opening_of`]).

use user::messenger::display::Rect;
use user::sys;

use super::anim::{trail_rects, trail_strips, zoom_total_ns, STRIPS};
use super::compositor::Compositor;

/// One window's open zoom in flight.
pub(super) struct Opening {
    /// The window, hidden (minimized) until the zoom lands.
    id: u64,
    /// The two moves: from the origin to an icon-sized rectangle centred on
    /// the window, then out to the window.
    legs: [(Rect, Rect); 2],
    /// The move being drawn.
    leg: usize,
    /// When it started (monotonic ns).
    started: u64,
    /// What the last frame drew, recomposed by the next one.
    previous: [Rect; STRIPS],
}

impl Compositor {
    /// Open window `id` with a zoom from `origin` (the rectangle the app
    /// hinted at) or else from its taskbar entry. With animations switched
    /// off the window simply shows.
    pub(super) fn start_opening(&mut self, id: u64, origin: Option<Rect>) {
        self.finish_opening();
        if !self.themefeed.animations() {
            return;
        }
        let Some((window, icon, small)) = self.phases(id) else {
            return;
        };
        let from = origin.unwrap_or(icon);
        let mut previous = [Rect::new(0, 0, 0, 0); STRIPS];
        previous[0] = from.intersect(self.full());
        self.set_minimized(id, true);
        self.opening = Some(Opening {
            id,
            legs: [(from, small), (small, window)],
            leg: 0,
            started: sys::monotonic_ns(),
            previous,
        });
    }

    /// Whether a window is opening: the main loop then parks at most a tick,
    /// so the zoom keeps its frame rate.
    pub(super) fn opening(&self) -> bool {
        self.opening.is_some()
    }

    /// Draw the opening zoom's frame for now; when it lands, show the window.
    pub(super) fn tick_opening(&mut self) {
        let Some(mut opening) = self.opening.take() else {
            return;
        };
        if !self.surfaces.iter().any(|surface| surface.id == opening.id) {
            // Closed while it flew in: erase the last outline.
            self.repaint_full();
            return;
        }
        let total = zoom_total_ns();
        let now = sys::monotonic_ns();
        let shown_at = now.saturating_sub(opening.started).min(total);
        let (from, to) = opening.legs[opening.leg];
        let rects = trail_rects(from, to, shown_at);
        let strips = trail_strips(&rects, self.full());
        self.draw_frame(&opening.previous, &strips, &rects);
        opening.previous = strips;
        if shown_at < total {
            self.opening = Some(opening);
        } else if opening.leg + 1 < opening.legs.len() {
            opening.leg += 1;
            opening.started = now;
            self.opening = Some(opening);
        } else {
            self.show_opened(opening.id);
        }
    }

    /// Land the zoom in flight at once (the window shows), if there is one.
    pub(super) fn finish_opening(&mut self) {
        if let Some(opening) = self.opening.take() {
            self.show_opened(opening.id);
        }
    }

    /// Land the zoom in flight if it is window `id`'s.
    pub(super) fn finish_opening_of(&mut self, id: u64) {
        if self
            .opening
            .as_ref()
            .is_some_and(|opening| opening.id == id)
        {
            self.finish_opening();
        }
    }

    /// Show the window an opening zoom hid; the full repaint also erases
    /// the last outline.
    fn show_opened(&mut self, id: u64) {
        self.set_minimized(id, false);
        self.repaint_full();
    }
}
