//! Compositing a window's node table and presenting the result, either through
//! the display grant (owner mode) or as a pipelined `Present` of a buffer slot
//! to `xuid` (client mode).
//!
//! Owner mode composes into the window's `frame` and copies that to the
//! screen. Client mode composes straight into the slot it presents (buffer
//! age, `Slots::prepare`): the slot first takes what it missed from the slot
//! composed last, so each damaged pixel is written once per frame.

use xui_canvas::Surface;
use xui_core::backend::{Canvas, Painter, WindowId};
use xui_core::Rect;

use crate::client_window::{copy_rect, ClientWindow, Slots};
use crate::display::{Client, FrameEvent};
use crate::sys::{self, DisplayInfo};

use super::geometry::{absolute_bounds, ancestor_clip, effectively_visible};
use super::{LazyOSBackend, Mode};

/// How far past its bounds a painter may draw (anti-aliasing, focus rings),
/// in pixels: nodes this close to the damage repaint with it.
const PAINT_SPILL: i32 = 2;

/// Where a composite writes the window's picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dest {
    /// The window's own `frame` (owner mode).
    Frame,
    /// A free buffer slot of the client window, prepared for the damage
    /// ([`Slots::prepare`]).
    Slot(u32),
}

impl LazyOSBackend {
    /// Repaints `damage` (window-absolute) of `window` into its composed
    /// frame: clears it to the background and runs, in creation order, only
    /// the painters of visible nodes that come near it. Pixels outside the
    /// damage keep the previous frame, so a whole-window `damage` is a full
    /// repaint (the first frame, a resize, a theme change).
    ///
    /// Everything is painted through one `Surface::paint_region` over the
    /// damage: painters draw at their real bounds, but only the damaged pixels
    /// are rasterised, so a hover over a big node costs the damage, not the
    /// node (#605). A canvas clip is not an option: `SkiaCanvas` trims each
    /// shape's rectangle to the clip before stroking it, so a node straddling
    /// the damage would get its border drawn along the damage edge; a region
    /// cuts pixels, not shapes. Pixels outside the damage keep the last frame.
    ///
    /// The surface is taken out of the window table before the painters run and
    /// put back afterwards: a painter can call back into the backend (the
    /// explorer queries `dpi`/`client_rect` while painting), which would panic
    /// on an already-borrowed `windows`. The window itself stays in the map, so
    /// those re-entrant reads still see its live size, DPI and theme.
    ///
    /// The result lands in `dest`; `false` when the window is gone, or when
    /// `dest` is a slot no longer of the window's size.
    fn composite(&self, window: WindowId, damage: Rect, dest: Dest) -> bool {
        // A node that keeps its own picture writes it straight into the destination (`direct.rs`).
        if let Some(done) = self.composite_direct(window, damage, dest) {
            return done;
        }
        self.composite_canvas(window, damage, dest)
    }

    /// [`composite`](Self::composite) through the canvas: every painter that comes near `damage`.
    pub(super) fn composite_canvas(&self, window: WindowId, damage: Rect, dest: Dest) -> bool {
        let (dpi, theme, backdrop, width, height, mut surface, mut background) = {
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
                std::mem::take(&mut entry.background),
            )
        };
        // Bounds are parent-relative: paint at the window-absolute position,
        // and skip a node hidden through any ancestor. A node just outside the
        // damage still runs: anti-aliased edges and focus rings spill a pixel
        // or two past a node's bounds. A node entirely outside an ancestor's
        // clip (a scroll view's row scrolled out of its viewport) is skipped:
        // painters are not clipped to it, so it would draw over its neighbours.
        let reach = inflate(damage, PAINT_SPILL * self.scale() as i32);
        let paints: Vec<(Rect, Painter, Option<Rect>)> = {
            let nodes = self.nodes.borrow();
            nodes
                .iter()
                .filter(|(id, node)| node.window == window && effectively_visible(&nodes, *id))
                .filter_map(|(id, node)| {
                    let painter = node.painter.clone()?;
                    let bounds = absolute_bounds(&nodes, *id)?;
                    let clip = ancestor_clip(&nodes, *id);
                    let shown = clip.is_none_or(|clip| intersects(bounds, clip));
                    // A node that sticks out of an ancestor's clip (a paragraph
                    // of a scroll view, scrolled half out of the viewport) is
                    // cut at it, as `xui-canvas`'s own compositor does; one
                    // inside it needs no clip.
                    let clip = clip.filter(|clip| !contains(*clip, bounds));
                    // A node that keeps its own picture (`direct.rs`) is opaque and spills nothing: its
                    // painter runs only for damage it overlaps (it is a whole frame through the canvas).
                    let near = if super::direct::is_direct(*id) {
                        damage
                    } else {
                        reach
                    };
                    (shown && intersects(bounds, near)).then_some((bounds, painter, clip))
                })
                .collect()
        };
        self.paint_damage.set(Some(damage));
        surface.paint_region(damage, |region| {
            // The window background (the theme's vertical gradient, spanning
            // the whole window so a partial repaint matches the rest), and over
            // it a backdrop (LazyShell's wallpaper) the widgets draw on like on
            // any container: one cached picture, copied into the damage.
            region.with_canvas_at(damage, dpi, |canvas| {
                background.paint(canvas, damage, (width, height), &theme, backdrop.as_deref());
            });
            // In creation order a container paints before the widgets in it,
            // so the widgets draw on it instead of filling their own
            // background.
            for (bounds, painter, clip) in paints {
                region.with_canvas_over_parents(bounds, dpi, |canvas| match clip {
                    // Window coordinates are the canvas's own (no transform
                    // is set yet), like the clip `xui-canvas` pushes.
                    Some(clip) => {
                        canvas.push_clip(clip);
                        painter(canvas);
                        canvas.pop_clip();
                    }
                    None => painter(canvas),
                });
            }
        });
        self.paint_damage.set(None);
        // Put the real surface back and fold the damage into the destination;
        // a painter that closed this window leaves no entry, so both drop.
        let mut windows = self.windows.borrow_mut();
        let Some(entry) = windows.get_mut(&window.raw()) else {
            return dest == Dest::Frame;
        };
        let pixels = surface.pixels();
        let written = match dest {
            Dest::Frame if entry.frame.len() == pixels.len() => {
                copy_rect(&mut entry.frame, pixels, width, height, damage);
                true
            }
            Dest::Frame => {
                // A new or resized window, which is always damaged whole.
                entry.frame = pixels.to_vec();
                true
            }
            Dest::Slot(slot) => entry
                .client
                .as_mut()
                .is_some_and(|client| client.slots.write(slot, pixels, width, height, damage)),
        };
        entry.surface = surface;
        entry.background = background;
        written
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
        if !self.composite(window, Rect::new(0, 0, width, height), Dest::Frame) {
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
    /// damage straight into it, after copying what that slot is missing from
    /// the slot composed last, and `Present` it. With no free slot the damage
    /// waits: the compositor's next `BufferRelease` paces the next frame.
    fn present_client(&self, window: WindowId, client: Client) -> bool {
        let Some((slot, full)) = self.acquire_slot(window, client) else {
            return false;
        };
        let damage = self.take_damage(window, full);
        let compose = self.with_slots(window, |slots| slots.prepare(slot, damage));
        let Some(compose) = compose.flatten() else {
            return false;
        };
        if !self.composite(window, compose, Dest::Slot(slot)) {
            // The window closed, or it was resized between two parts of the
            // composite: the slot holds part of a frame, so it and the window
            // repaint whole.
            if self
                .with_slots(window, |slots| slots.invalidate(slot))
                .is_some()
            {
                self.add_damage(window, full);
            }
            return false;
        }
        let submitted = self
            .windows
            .borrow_mut()
            .get_mut(&window.raw())
            .and_then(|entry| entry.client.as_mut())
            .and_then(|surface| Some((surface.surface, surface.slots.submit(slot)?)));
        let Some((surface, seq)) = submitted else {
            // A window closed by a painter keeps no damage.
            if self.with_slots(window, |_| ()).is_some() {
                self.add_damage(window, full);
            }
            return false;
        };
        if client.present(surface, slot, seq, compose).is_ok() {
            return true;
        }
        // The compositor never saw this present, so it would never release
        // the slot: take it back and keep the damage for the next tick.
        self.with_slots(window, |slots| slots.cancel(slot, seq));
        self.add_damage(window, compose);
        false
    }

    /// A free slot of `window` of its current size, and its full rectangle.
    fn acquire_slot(&self, window: WindowId, client: Client) -> Option<(u32, Rect)> {
        let mut windows = self.windows.borrow_mut();
        let entry = windows.get_mut(&window.raw())?;
        let (width, height) = (entry.width, entry.height);
        let surface = entry.client.as_mut()?;
        let slot = surface
            .slots
            .acquire(client, surface.surface, width, height)
            .ok()??;
        Some((slot, Rect::new(0, 0, width, height)))
    }

    /// Destroy a closed client window's surface and release its buffers.
    pub(super) fn close_client_window(&self, surface: ClientWindow) {
        if let Mode::Client(state) = &self.mode {
            surface.close(state.borrow().client);
        }
    }

    /// Run `f` on `window`'s slots; `None` when it has none (closed, or not a
    /// client window).
    pub(super) fn with_slots<T>(
        &self,
        window: WindowId,
        f: impl FnOnce(&mut Slots) -> T,
    ) -> Option<T> {
        let mut windows = self.windows.borrow_mut();
        let surface = windows.get_mut(&window.raw())?.client.as_mut()?;
        Some(f(&mut surface.slots))
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

    /// `rect` grown by the margin a painter may draw past its node's bounds
    /// ([`PAINT_SPILL`]): what a node that appears or goes must repaint, or
    /// its border and anti-aliased edges stay behind on screen.
    pub(super) fn with_spill(&self, rect: Rect) -> Rect {
        inflate(rect, PAINT_SPILL * self.scale() as i32)
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

/// Whether `outer` holds every pixel of `inner`.
fn contains(outer: Rect, inner: Rect) -> bool {
    outer.left <= inner.left
        && outer.top <= inner.top
        && inner.right <= outer.right
        && inner.bottom <= outer.bottom
}

#[cfg(test)]
mod tests;
