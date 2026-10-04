//! Where the window's parts go: a toolbar (Back, Forward, Reload, the
//! address field, Go) across the top, a status line along the bottom, and the
//! page in between. Sizes are design pixels times the UI `scale`.

use xui_core::Rect;

/// Toolbar height (design pixels).
pub const TOOLBAR_H: i32 = 36;
/// Status line height (design pixels).
pub const STATUS_H: i32 = 24;

/// The window's widget rectangles, in screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub back: Rect,
    pub forward: Rect,
    pub reload: Rect,
    pub address: Rect,
    pub go: Rect,
    pub view: Rect,
    pub status: Rect,
}

/// The layout for a client area of `client` (screen pixels) at `scale`.
pub fn layout(client: Rect, scale: i32) -> Layout {
    let s = scale.max(1);
    let width = client.width().max(1);
    let height = client.height().max(1);
    let px = |x: i32, y: i32, w: i32, h: i32| Rect::new(x * s, y * s, (x + w) * s, (y + h) * s);
    let w = width / s;
    let back = px(6, 5, 52, 26);
    let forward = px(62, 5, 64, 26);
    let reload = px(130, 5, 60, 26);
    let go = px((w - 46).max(240), 5, 40, 26);
    let address_left = 196;
    let address = px(
        address_left,
        5,
        (go.left / s - 6 - address_left).max(40),
        26,
    );
    let top = (TOOLBAR_H * s).min(height);
    let bottom = (height - STATUS_H * s).max(top);
    let view = Rect::new(0, top, width, bottom.max(top + 1));
    let status_top = bottom + 3 * s;
    let status = Rect::new(
        8 * s,
        status_top,
        (width - 8 * s).max(8 * s + 1),
        height.max(status_top + 1),
    );
    Layout {
        back,
        forward,
        reload,
        address,
        go,
        view,
        status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_fills_between_toolbar_and_status() {
        let l = layout(Rect::new(0, 0, 1000, 700), 1);
        assert_eq!(l.view, Rect::new(0, 36, 1000, 676));
        assert_eq!(l.status.top, 679);
        assert_eq!(l.go.right, 1000 - 6);
        assert_eq!(l.address.right, l.go.left - 6);
        assert!(l.address.left > l.reload.right);
    }

    #[test]
    fn hidpi_doubles_everything() {
        let one = layout(Rect::new(0, 0, 1000, 700), 1);
        let two = layout(Rect::new(0, 0, 2000, 1400), 2);
        assert_eq!(two.view, Rect::new(0, 72, 2000, 1352));
        assert_eq!(two.back.width(), one.back.width() * 2);
        assert_eq!(two.address.width(), one.address.width() * 2);
    }

    #[test]
    fn a_tiny_window_still_has_positive_rects() {
        let l = layout(Rect::new(0, 0, 50, 20), 1);
        for r in [l.address, l.view, l.status] {
            assert!(r.width() > 0 && r.height() > 0, "{r:?}");
        }
    }
}
