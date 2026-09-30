//! Compositing a window's node table and presenting the result, either through
//! the display grant (owner mode) or as a damage commit to `xuid` (client).

use xui_canvas::Surface;
use xui_core::backend::{Painter, WindowId};
use xui_core::Rect;

use crate::sys;

use super::geometry::{absolute_bounds, effectively_visible};
use super::{LazyOSBackend, Mode};

impl LazyOSBackend {
    /// Composites `window`'s visible nodes, in creation order, into its
    /// surface, ready for [`LazyOSBackend::present`] to copy.
    ///
    /// The surface is taken out of the window table before the painters run and
    /// put back afterwards: a painter can call back into the backend (the
    /// explorer queries `dpi`/`client_rect` while painting), which would panic
    /// on an already-borrowed `windows`. The window itself stays in the map, so
    /// those re-entrant reads still see its live size, DPI and theme.
    fn composite(&self, window: WindowId) -> bool {
        let (dpi, background, mut surface) = {
            let mut windows = self.windows.borrow_mut();
            let Some(entry) = windows.get_mut(&window.raw()) else {
                return false;
            };
            (
                entry.dpi,
                entry.background,
                std::mem::replace(&mut entry.surface, Surface::new(1, 1)),
            )
        };
        surface.fill(background);
        // Bounds are parent-relative: paint at the window-absolute position,
        // and skip a node hidden through any ancestor.
        let paints: Vec<(Rect, Painter)> = {
            let nodes = self.nodes.borrow();
            nodes
                .iter()
                .filter(|(id, node)| node.window == window && effectively_visible(&nodes, *id))
                .filter_map(|(id, node)| {
                    let painter = node.painter.clone()?;
                    Some((absolute_bounds(&nodes, *id)?, painter))
                })
                .collect()
        };
        for (bounds, painter) in paints {
            surface.with_canvas_at(bounds, dpi, |canvas| painter(canvas));
        }
        // Put the real surface back; a painter that closed this window leaves
        // no entry, so the surface simply drops.
        if let Some(entry) = self.windows.borrow_mut().get_mut(&window.raw()) {
            entry.surface = surface;
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
                let client = state.borrow().client;
                let mut windows = self.windows.borrow_mut();
                let Some(entry) = windows.get_mut(&window.raw()) else {
                    return false;
                };
                let Some(surface) = entry.client.as_ref() else {
                    return false;
                };
                let full = Rect::new(0, 0, surface.rect.0, surface.rect.1);
                let damage = self.take_damage(window, full);
                let pixels = entry.surface.pixels();
                if surface.va == 0 || pixels.len() > surface.size as usize {
                    return false;
                }
                // Safety: `va`/`size` describe the shared buffer
                // `display_create_buffer` mapped into this task; `pixels` is
                // the window-sized RGBA image and fits inside it.
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        pixels.as_ptr(),
                        surface.va as *mut u8,
                        pixels.len(),
                    );
                }
                let surface_id = surface.surface;
                drop(windows);
                if client
                    .commit(
                        surface_id,
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

    /// The damage `window` accumulated since the last commit, clamped to `full`.
    pub(super) fn take_damage(&self, window: WindowId, full: Rect) -> Rect {
        let Some(damage) = self.damage.borrow_mut().remove(&window.raw()) else {
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

    /// Grow `window`'s pending damage by `rect`.
    pub(super) fn add_damage(&self, window: WindowId, rect: Rect) {
        let mut damage = self.damage.borrow_mut();
        let merged = match damage.get(&window.raw()) {
            Some(existing) => Rect::new(
                existing.left.min(rect.left),
                existing.top.min(rect.top),
                existing.right.max(rect.right),
                existing.bottom.max(rect.bottom),
            ),
            None => rect,
        };
        damage.insert(window.raw(), merged);
    }
}
