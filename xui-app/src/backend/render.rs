//! Compositing a window's node table and presenting the result, either through
//! the display grant (owner mode) or as a damage commit to `xuid` (client).

use xui_core::backend::{Painter, WindowId};
use xui_core::Rect;

use crate::sys;

use super::{LazyOSBackend, Mode};

impl LazyOSBackend {
    /// Composites `window`'s visible nodes, in creation order, into its
    /// surface, ready for [`LazyOSBackend::present`] to copy.
    fn composite(&self, window: WindowId) -> bool {
        let mut windows = self.windows.borrow_mut();
        let Some(entry) = windows.get_mut(&window.raw()) else {
            return false;
        };
        entry.surface.fill(entry.background);
        let dpi = entry.dpi;
        let paints: Vec<(Rect, Painter)> = self
            .nodes
            .borrow()
            .iter()
            .filter(|(_, node)| node.window == window && node.visible)
            .filter_map(|(_, node)| node.painter.clone().map(|painter| (node.bounds, painter)))
            .collect();
        for (bounds, painter) in paints {
            entry
                .surface
                .with_canvas_at(bounds, dpi, |canvas| painter(canvas));
        }
        true
    }

    /// Render and blit the window: a damage-rectangle present through the
    /// display grant (owner mode), or a damage-rectangle commit to the
    /// compositor (client mode).
    pub(super) fn present(&self, window: WindowId) -> bool {
        if !self.composite(window) {
            return false;
        }
        match &self.mode {
            Mode::Owner { display } => {
                let (width, height) = self.screen();
                let size = display.size as usize;
                let mut windows = self.windows.borrow_mut();
                let Some(entry) = windows.get_mut(&window.raw()) else {
                    return false;
                };
                let pixels = entry.surface.pixels();
                if pixels.len() != size {
                    return false;
                }
                // Safety: `va`/`size` are the mapping the kernel installed for
                // this task's screen buffer at bind; `pixels` is exactly
                // `size` bytes.
                unsafe {
                    core::ptr::copy_nonoverlapping(pixels.as_ptr(), display.va as *mut u8, size);
                }
                if sys::display_present(0, 0, width, height).is_err() {
                    return false;
                }
            }
            Mode::Client(state) => {
                let state = state.borrow();
                let full = Rect::new(0, 0, state.rect.0, state.rect.1);
                let damage = self.take_damage(full);
                let mut windows = self.windows.borrow_mut();
                let Some(entry) = windows.get_mut(&window.raw()) else {
                    return false;
                };
                let pixels = entry.surface.pixels();
                if state.va == 0 || pixels.len() > state.size as usize {
                    return false;
                }
                // Safety: `va`/`size` describe the shared buffer
                // `display_create_buffer` mapped into this task; `pixels` is
                // the window-sized RGBA image and fits inside it.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        pixels.as_ptr(),
                        state.va as *mut u8,
                        pixels.len(),
                    );
                }
                drop(windows);
                if state
                    .client
                    .commit(
                        state.surface,
                        (damage.left, damage.top, damage.width(), damage.height()),
                    )
                    .is_err()
                {
                    return false;
                }
            }
        }
        self.frames.set(self.frames.get() + 1);
        if self.frames.get() == 1 {
            if let Some(callback) = self.on_first_frame.borrow_mut().take() {
                callback();
            }
        }
        true
    }

    /// The damage accumulated since the last commit, clamped to `full`.
    fn take_damage(&self, full: Rect) -> Rect {
        let Some(damage) = self.damage.take() else {
            return full;
        };
        let clipped = Rect::new(
            damage.left.max(full.left),
            damage.top.max(full.top),
            damage.right.min(full.right),
            damage.bottom.min(full.bottom),
        );
        if clipped.is_empty() {
            full
        } else {
            clipped
        }
    }

    /// Grow the pending damage by `rect`.
    pub(super) fn add_damage(&self, rect: Rect) {
        let merged = match self.damage.get() {
            Some(existing) => Rect::new(
                existing.left.min(rect.left),
                existing.top.min(rect.top),
                existing.right.max(rect.right),
                existing.bottom.max(rect.bottom),
            ),
            None => rect,
        };
        self.damage.set(Some(merged));
    }
}
