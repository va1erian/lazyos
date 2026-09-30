//! The open-origin hint: tell `xuid` where the next window should zoom open
//! from (the folder tile the user just double-clicked).
//!
//! The portable explorer only knows a tile size; the tile itself is the one
//! under the pointer, whose last window-relative position the backend tracks.
//! The hint is cosmetic, so every failure (owner mode, no surface, pointer
//! outside the window, an older compositor) is silently ignored.

use xui_core::backend::WindowId;

use super::{LazyOSBackend, Mode};

/// The hint rectangle `(x, y, w, h)`: a `tile`-pixel square centred on
/// `pointer`, both relative to the window content, or `None` when the pointer
/// is outside a `client`-sized window (a stale position from a keyboard
/// activation must not start the animation from empty space) or `tile` is not
/// positive.
pub(super) fn tile_around(
    pointer: (i32, i32),
    tile: i32,
    client: (i32, i32),
) -> Option<(i32, i32, u32, u32)> {
    let inside = pointer.0 >= 0 && pointer.1 >= 0 && pointer.0 < client.0 && pointer.1 < client.1;
    if tile <= 0 || !inside {
        return None;
    }
    let half = tile / 2;
    Some((pointer.0 - half, pointer.1 - half, tile as u32, tile as u32))
}

impl LazyOSBackend {
    /// Ask the compositor to open this task's next window from the tile of
    /// `tile` device pixels under the pointer in `window`. A no-op in owner
    /// mode (no compositor) and for a window without a surface.
    pub fn hint_open_origin(&self, window: WindowId, tile: i32) {
        let Mode::Client(state) = &self.mode else {
            return;
        };
        let client = state.borrow().client;
        let target = {
            let windows = self.windows.borrow();
            windows.get(&window.raw()).and_then(|entry| {
                let surface = entry.client.as_ref()?.surface;
                Some((surface, (entry.width, entry.height)))
            })
        };
        let Some((surface, size)) = target else {
            return;
        };
        if let Some(rect) = tile_around(self.pointer.get(), tile, size) {
            let _ = client.hint_open_origin(surface, rect);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tile_is_centred_on_the_pointer() {
        assert_eq!(
            tile_around((110, 95), 64, (720, 480)),
            Some((78, 63, 64, 64))
        );
    }

    #[test]
    fn a_pointer_outside_the_window_or_an_empty_tile_gives_no_hint() {
        assert_eq!(tile_around((-1, 10), 64, (720, 480)), None);
        assert_eq!(tile_around((720, 10), 64, (720, 480)), None);
        assert_eq!(tile_around((10, 480), 64, (720, 480)), None);
        assert_eq!(tile_around((10, 10), 0, (720, 480)), None);
        assert_eq!(tile_around((10, 10), -5, (720, 480)), None);
    }

    #[test]
    fn a_window_without_a_surface_is_ignored() {
        let backend = crate::backend::test_support::client_backend();
        backend.hint_open_origin(WindowId::from_raw(1), 64);
        backend.hint_open_origin(WindowId::from_raw(99), 64);
    }
}
