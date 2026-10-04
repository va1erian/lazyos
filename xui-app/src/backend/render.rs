//! Compositing a window's node table and presenting the result, either through
//! the display grant (owner mode) or as a pipelined `Present` of a buffer slot
//! to `xuid` (client mode).

use xui_canvas::Surface;
use xui_core::backend::{Painter, WindowId};
use xui_core::theme::look;
use xui_core::Rect;

use crate::client_window::copy_rect;
use crate::display::{Client, FrameEvent};
use crate::sys::{self, DisplayInfo};

use super::backdrop;
use super::geometry::{absolute_bounds, effectively_visible};
use super::{LazyOSBackend, Mode};

/// How far past its bounds a painter may draw (anti-aliasing, focus rings),
/// in pixels: nodes this close to the damage repaint with it.
const PAINT_SPILL: i32 = 2;

impl LazyOSBackend {
    /// Repaints `damage` (window-absolute) of `window` into its composed
    /// frame: clears it to the background and runs, in creation order, only
    /// the painters of visible nodes that come near it. Pixels outside the
    /// damage keep the previous frame, so a whole-window `damage` is a full
    /// repaint (the first frame, a resize, a theme change).
    ///
    /// Painters run unclipped on the painting surface and only the damaged
    /// rectangle is copied into the frame. A canvas clip is not an option:
    /// `SkiaCanvas` trims each shape's rectangle to the clip before stroking
    /// it, so a node straddling the damage would get its border drawn along
    /// the damage edge. Unclipped, every pixel inside the damage is exactly
    /// what a full repaint produces; what a straddling node overdraws outside
    /// it never reaches the frame.
    ///
    /// The surface is taken out of the window table before the painters run and
    /// put back afterwards: a painter can call back into the backend (the
    /// explorer queries `dpi`/`client_rect` while painting), which would panic
    /// on an already-borrowed `windows`. The window itself stays in the map, so
    /// those re-entrant reads still see its live size, DPI and theme.
    fn composite(&self, window: WindowId, damage: Rect) -> bool {
        let (dpi, theme, backdrop, width, height, mut surface) = {
            let mut windows = self.windows.borrow_mut();
            let Some(entry) = windows.get_mut(&window.raw()) else {
                return false;
            };
            (
                entry.dpi,
                entry.theme,
                entry.backdrop.clone(),
                entry.width,
                entry.height,
                std::mem::replace(&mut entry.surface, Surface::new(1, 1)),
            )
        };
        // The window background (the theme's vertical gradient, spanning the
        // whole window so a partial repaint matches the rest).
        let window_rect = Rect::new(0, 0, width, height);
        surface.with_canvas_at(damage, dpi, |canvas| {
            look::paint_background(canvas, damage, window_rect, &theme);
            // A backdrop (LazyShell's wallpaper) covers it; the widgets then
            // draw on the picture like on any container.
            if let Some(image) = &backdrop {
                backdrop::draw(canvas, image, window_rect, damage);
            }
        });
        // Bounds are parent-relative: paint at the window-absolute position,
        // and skip a node hidden through any ancestor. A node just outside the
        // damage still runs: anti-aliased edges and focus rings spill a pixel
        // or two past a node's bounds.
        let reach = inflate(damage, PAINT_SPILL * self.scale() as i32);
        let paints: Vec<(Rect, Painter)> = {
            let nodes = self.nodes.borrow();
            nodes
                .iter()
                .filter(|(id, node)| node.window == window && effectively_visible(&nodes, *id))
                .filter_map(|(id, node)| {
                    let painter = node.painter.clone()?;
                    let bounds = absolute_bounds(&nodes, *id)?;
                    intersects(bounds, reach).then_some((bounds, painter))
                })
                .collect()
        };
        // In creation order a container paints before the widgets in it, so
        // the widgets draw on it instead of filling their own background.
        for (bounds, painter) in paints {
            surface.with_canvas_over_parents(bounds, dpi, |canvas| painter(canvas));
        }
        // Put the real surface back and fold the damage into the frame; a
        // painter that closed this window leaves no entry, so both drop.
        let mut windows = self.windows.borrow_mut();
        let Some(entry) = windows.get_mut(&window.raw()) else {
            return true;
        };
        let pixels = surface.pixels();
        if entry.frame.len() == pixels.len() {
            copy_rect(&mut entry.frame, pixels, width, height, damage);
        } else {
            // A new or resized window, which is always damaged whole.
            entry.frame = pixels.to_vec();
        }
        entry.surface = surface;
        true
    }

    /// Render and show the window: a full-screen present through the display
    /// grant (owner mode), or a damage-only `Present` to the compositor
    /// (client mode). `false` when nothing reached the screen; a client's
    /// damage then stays pending for the next tick.
    pub(super) fn present(&self, window: WindowId) -> bool {
        let presented = match &self.mode {
            Mode::Owner { display } => self.present_owner(window, display),
            Mode::Client(state) => self.present_client(window, state.borrow().client),
        };
        if !presented {
            return false;
        }
        self.frames.set(self.frames.get() + 1);
        if self.frames.get() == 1 {
            if let Some(callback) = self.on_first_frame.borrow_mut().take() {
                callback();
            }
        }
        true
    }

    /// Owner mode: repaint the whole screen and blit it through the grant.
    /// Owner mode tracks no damage (the backend only records it for clients).
    fn present_owner(&self, window: WindowId, display: &DisplayInfo) -> bool {
        let (width, height) = self.screen();
        if !self.composite(window, Rect::new(0, 0, width, height)) {
            return false;
        }
        let size = display.size as usize;
        let windows = self.windows.borrow();
        let Some(entry) = windows.get(&window.raw()) else {
            return false;
        };
        let pixels = &entry.frame;
        if pixels.len() != size {
            return false;
        }
        // Safety: `va`/`size` are the mapping the kernel installed for this
        // task's screen buffer at bind; `pixels` is exactly `size` bytes.
        unsafe {
            core::ptr::copy_nonoverlapping(pixels.as_ptr(), display.va as *mut u8, size);
        }
        sys::display_present(0, 0, width, height).is_ok()
    }

    /// Client mode: once a buffer slot is free, repaint only the pending
    /// damage, copy only what that slot is missing into it, and `Present` it.
    /// With no free slot the damage waits: the compositor's next
    /// `BufferRelease` paces the next frame.
    fn present_client(&self, window: WindowId, client: Client) -> bool {
        let (slot, full) = {
            let mut windows = self.windows.borrow_mut();
            let Some(entry) = windows.get_mut(&window.raw()) else {
                return false;
            };
            let (width, height) = (entry.width, entry.height);
            let Some(surface) = entry.client.as_mut() else {
                return false;
            };
            match surface
                .slots
                .acquire(client, surface.surface, width, height)
            {
                Ok(Some(slot)) => (slot, Rect::new(0, 0, width, height)),
                _ => return false,
            }
        };
        let damage = self.take_damage(window, full);
        if !self.composite(window, damage) {
            return false;
        }
        let submitted = {
            let mut windows = self.windows.borrow_mut();
            let Some(entry) = windows.get_mut(&window.raw()) else {
                return false;
            };
            let pixels = &entry.frame;
            entry.client.as_mut().and_then(|surface| {
                surface.slots.damage(damage);
                if !surface.slots.sync(slot, pixels) {
                    return None;
                }
                Some((surface.surface, surface.slots.submit(slot)?))
            })
        };
        let Some((surface, seq)) = submitted else {
            // The surface changed size under the painters; repaint it whole.
            self.add_damage(window, full);
            return false;
        };
        if client.present(surface, slot, seq, damage).is_ok() {
            return true;
        }
        // The compositor never saw this present, so it would never release
        // the slot: take it back and keep the damage for the next tick.
        if let Some(surface) = self
            .windows
            .borrow_mut()
            .get_mut(&window.raw())
            .and_then(|entry| entry.client.as_mut())
        {
            surface.slots.cancel(slot, seq);
        }
        self.add_damage(window, damage);
        false
    }

    /// Fold a `BufferRelease` or `FrameDone` for `window` into its slots. A
    /// refused present never reached the screen, so the window repaints whole.
    pub(super) fn frame_event(&self, window: WindowId, event: FrameEvent) {
        let refused = {
            let mut windows = self.windows.borrow_mut();
            let Some(entry) = windows.get_mut(&window.raw()) else {
                return;
            };
            let full = Rect::new(0, 0, entry.width, entry.height);
            let Some(surface) = entry.client.as_mut() else {
                return;
            };
            match event {
                FrameEvent::BufferRelease { slot } => surface.slots.released(slot).then_some(full),
                FrameEvent::FrameDone { seq } => {
                    surface.slots.frame_done(seq);
                    None
                }
            }
        };
        if let Some(full) = refused {
            self.add_damage(window, full);
        }
    }

    /// The damage `window` accumulated since the last present, clamped to
    /// `full`.
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

/// `rect` grown by `by` pixels on every side.
fn inflate(rect: Rect, by: i32) -> Rect {
    Rect::new(
        rect.left - by,
        rect.top - by,
        rect.right + by,
        rect.bottom + by,
    )
}

/// Whether `a` and `b` share at least one pixel.
fn intersects(a: Rect, b: Rect) -> bool {
    a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use xui_core::backend::{Canvas, Event, ParentRef, WidgetId};
    use xui_core::router::WidgetHost;
    use xui_core::Color;

    use super::super::test_support::client_backend;
    use super::super::{Node, Window};
    use super::*;

    const W: WindowId = WindowId::from_raw(1);
    const RED: Color = Color::rgb(200, 0, 0);
    const BLUE: Color = Color::rgb(0, 0, 200);

    struct Sink;

    impl WidgetHost for Sink {
        fn deliver(&self, _target: WidgetId, _event: &Event) -> bool {
            true
        }
    }

    /// A node at `bounds` whose painter fills it with `color` and counts its
    /// runs in `calls`.
    fn painted(bounds: Rect, color: Color, calls: Rc<Cell<u32>>) -> Node {
        let painter: Painter = Rc::new(move |canvas: &mut dyn Canvas| {
            calls.set(calls.get() + 1);
            canvas.fill_rect(canvas.bounds(), color);
        });
        Node {
            window: W,
            parent: ParentRef::Window(W),
            bounds,
            visible: true,
            enabled: true,
            focus_stop: false,
            clip: None,
            text: String::new(),
            painter: Some(painter),
        }
    }

    /// The pixel at `(x, y)` of the window's surface.
    fn pixel(backend: &LazyOSBackend, x: u32, y: u32) -> [u8; 4] {
        let windows = backend.windows.borrow();
        let at = ((y * 64 + x) * 4) as usize;
        windows[&W.raw()].frame[at..at + 4]
            .try_into()
            .expect("in bounds")
    }

    fn rgba(color: Color) -> [u8; 4] {
        [color.r, color.g, color.b, 255]
    }

    /// A 64x64 window with a red node on the left and a blue node on the
    /// right, painted once in full; the per-node paint counters.
    fn rig() -> (LazyOSBackend, Rc<Cell<u32>>, Rc<Cell<u32>>) {
        let backend = client_backend();
        backend
            .windows
            .borrow_mut()
            .insert(W.raw(), Window::for_tests(Rc::new(Sink)));
        let (left, right) = (Rc::new(Cell::new(0)), Rc::new(Cell::new(0)));
        let mut nodes = backend.nodes.borrow_mut();
        nodes.push((
            WidgetId::from_raw(1),
            painted(Rect::new(0, 0, 32, 64), RED, left.clone()),
        ));
        nodes.push((
            WidgetId::from_raw(2),
            painted(Rect::new(32, 0, 64, 64), BLUE, right.clone()),
        ));
        drop(nodes);
        assert!(backend.composite(W, Rect::new(0, 0, 64, 64)));
        assert_eq!((left.get(), right.get()), (1, 1));
        (backend, left, right)
    }

    #[test]
    fn a_painter_outside_the_damage_is_not_run() {
        let (backend, left, right) = rig();
        assert!(backend.composite(W, Rect::new(4, 4, 20, 20)));
        assert_eq!(left.get(), 2, "the damaged node repainted");
        assert_eq!(right.get(), 1, "the undamaged node did not");
    }

    #[test]
    fn pixels_outside_the_damage_are_untouched() {
        let (backend, _, _) = rig();
        // Recolour the left node, then repaint only part of it.
        backend.nodes.borrow_mut()[0].1 = painted(Rect::new(0, 0, 32, 64), BLUE, Rc::default());
        assert!(backend.composite(W, Rect::new(4, 4, 20, 20)));
        assert_eq!(pixel(&backend, 10, 10), rgba(BLUE), "inside the damage");
        assert_eq!(pixel(&backend, 2, 2), rgba(RED), "outside: the old frame");
        assert_eq!(pixel(&backend, 25, 30), rgba(RED), "outside: the old frame");
    }

    #[test]
    fn a_painter_straddling_the_damage_cannot_overdraw_its_neighbour() {
        let (backend, left, right) = rig();
        // Make the left node overlap the right one; only a strip clear of the
        // right node is damaged, so the right node's pixels must survive the
        // left one painting over them on the surface.
        backend.nodes.borrow_mut()[0].1 = painted(Rect::new(0, 0, 48, 64), RED, left.clone());
        assert!(backend.composite(W, Rect::new(0, 0, 24, 64)));
        assert_eq!(right.get(), 1);
        assert_eq!(pixel(&backend, 40, 10), rgba(BLUE));
        assert_eq!(pixel(&backend, 20, 10), rgba(RED));
        assert_eq!(pixel(&backend, 28, 10), rgba(RED), "outside: the old frame");
    }

    #[test]
    fn a_node_within_the_spill_margin_repaints_with_the_damage() {
        let (backend, _, right) = rig();
        // The right node starts at x = 32, one pixel past this damage.
        assert!(backend.composite(W, Rect::new(10, 10, 31, 20)));
        assert_eq!(right.get(), 2);
    }

    #[test]
    fn the_damage_is_cleared_to_the_background_first() {
        let (backend, _, _) = rig();
        backend.nodes.borrow_mut().remove(0);
        assert!(backend.composite(W, Rect::new(0, 0, 32, 64)));
        let background = backend.windows.borrow()[&W.raw()].theme.background;
        assert_eq!(pixel(&backend, 10, 10), rgba(background));
        assert_eq!(pixel(&backend, 40, 10), rgba(BLUE));
    }
}
