//! xuid UI constants (issue #194 split): window geometry and the chrome
//! colour palette. The palette is live: `themefeed` resolves the
//! `sys/ui/*` settings (`libs/uitheme`) into it, and every colour below is an
//! accessor reading the current value.

use core::sync::atomic::{AtomicU32, Ordering};
use uitheme::Palette;
use user::messenger::display::Color;

/// Title-bar height in pixels.
pub(super) const TITLE_H: i32 = 22;
/// Window border thickness in pixels.
pub(super) const BORDER: i32 = 2;
/// Where the first window's top-left sits.
pub(super) const PAD: i32 = 48;
/// Gap between tiled windows (issue #250).
pub(super) const WINDOW_GAP: i32 = 16;
/// Offset added per full grid of windows when placement must cascade (issue
/// #250).
pub(super) const CASCADE_STEP: i32 = 32;
/// The width of a cascaded window kept on screen: enough to show its title
/// bar and grab it, even when the window itself is past the right edge.
pub(super) const CASCADE_VISIBLE_W: i32 = 240;
/// The size of the rectangle a window without shell icon geometry
/// (`SetIconGeometry`) zooms to and from, at the screen's bottom-left.
pub(super) const ICON_W: i32 = 48;
pub(super) const ICON_H: i32 = 20;
/// Title-bar button size in pixels.
pub(super) const BUTTON: i32 = 16;
/// Gap between the two title-bar buttons.
pub(super) const BUTTON_GAP: i32 = 2;
/// Distance from the button group to the window's right edge.
pub(super) const BUTTON_MARGIN: i32 = 3;
/// Interactive-resize frame grip: how far inside (and outside) a window side a
/// pointer grabs that edge.
pub(super) const RESIZE_GRIP: i32 = 4;
/// How far outside the window rectangle a pointer still counts as on its
/// frame; the outer half of the grip.
pub(super) const RESIZE_OUT: i32 = 2;
/// Corner grip span: how far along a side a point still grabs the corner, so
/// diagonal resizing is easy to hit.
pub(super) const CORNER_GRIP: i32 = 14;
/// Smallest content width a resizable window may have, in pixels: enough for
/// the three title-bar buttons plus a little title.
pub(super) const MIN_CONTENT_W: i32 = 120;
/// Smallest content height a resizable window may have, in pixels.
pub(super) const MIN_CONTENT_H: i32 = 40;
/// How much of a window's title bar must stay on screen when it is moved off
/// an edge, so it can be grabbed again.
pub(super) const TITLE_REACHABLE_W: i32 = 64;
/// Two title-bar presses within this many PIT ticks (10 ms each) are a
/// double-click (500 ms).
pub(super) const DOUBLE_CLICK_TICKS: u64 = 50;
/// Pointer slop, in pixels, allowed between the two presses of a
/// double-click.
pub(super) const DOUBLE_CLICK_SLOP: i32 = 4;

/// Drop-target frame and drag-label accent (issue #145).
pub(super) const DRAG_ACCENT: Color = Color::rgb(245, 196, 84);
/// The drag label's chip background.
pub(super) const DRAG_GHOST_BG: Color = Color::rgb(28, 24, 12);

/// One slot per [`Palette`] field `xuid` uses, in declaration order. `xuid`
/// is a single task, so relaxed atomics are only there to keep the static
/// safe.
static SLOTS: [AtomicU32; 13] = [const { AtomicU32::new(0) }; 13];

fn slots(p: &Palette) -> [u32; 13] {
    [
        p.background,
        p.window_bg,
        p.title_bg,
        p.title_bg_focus,
        p.title_text,
        p.border,
        p.border_focus,
        p.empty_bg,
        p.taskbar_bg,
        p.overlay_bg,
        p.overlay_border,
        p.overlay_selected,
        p.overlay_text,
    ]
}

/// Install `palette`; `true` when any colour changed.
pub(super) fn set_palette(palette: &Palette) -> bool {
    let mut changed = false;
    for (slot, value) in SLOTS.iter().zip(slots(palette)) {
        changed |= slot.swap(value, Ordering::Relaxed) != value;
    }
    changed
}

fn color(index: usize) -> Color {
    let v = SLOTS[index].load(Ordering::Relaxed);
    Color::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

pub(super) fn background() -> Color {
    color(0)
}
pub(super) fn window_bg() -> Color {
    color(1)
}
pub(super) fn title_bg() -> Color {
    color(2)
}
pub(super) fn title_bg_focus() -> Color {
    color(3)
}
pub(super) fn title_text() -> Color {
    color(4)
}
pub(super) fn border_color() -> Color {
    color(5)
}
pub(super) fn border_color_focus() -> Color {
    color(6)
}
pub(super) fn empty_bg() -> Color {
    color(7)
}
/// The theme's taskbar colour: `xuid` paints no taskbar since issue #157,
/// but `GetTheme` still reports it so the shell can match.
pub(super) fn taskbar_bg() -> Color {
    color(8)
}
pub(super) fn overlay_bg() -> Color {
    color(9)
}
pub(super) fn overlay_border() -> Color {
    color(10)
}
pub(super) fn overlay_selected() -> Color {
    color(11)
}
pub(super) fn overlay_text() -> Color {
    color(12)
}
