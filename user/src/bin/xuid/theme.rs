//! xuid UI constants (issue #194 split): window/taskbar geometry and the
//! chrome colour palette. The palette is live: `themefeed` resolves the
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
/// Taskbar height in pixels.
pub(super) const TASKBAR_H: i32 = 28;
/// Gap between tiled windows (issue #250).
pub(super) const WINDOW_GAP: i32 = 16;
/// Offset added per full grid of windows when placement must cascade (issue
/// #250).
pub(super) const CASCADE_STEP: i32 = 32;
/// The width of a cascaded window kept on screen: enough to show its title
/// bar and grab it, even when the window itself is past the right edge.
pub(super) const CASCADE_VISIBLE_W: i32 = 240;
/// Taskbar entry height in pixels.
pub(super) const ENTRY_H: i32 = 20;
/// Horizontal gap between taskbar entries.
pub(super) const ENTRY_GAP: i32 = 4;
/// Taskbar margin before the first and after the last entry.
pub(super) const ENTRY_MARGIN: i32 = 6;
/// Horizontal padding inside a taskbar entry, per side.
pub(super) const ENTRY_PAD: i32 = 8;
/// Smallest taskbar entry width.
pub(super) const ENTRY_MIN_W: i32 = 48;
/// Title-bar button size in pixels.
pub(super) const BUTTON: i32 = 16;
/// Gap between the two title-bar buttons.
pub(super) const BUTTON_GAP: i32 = 2;
/// Distance from the button group to the window's right edge.
pub(super) const BUTTON_MARGIN: i32 = 3;

/// Drop-target frame and drag-label accent (issue #145).
pub(super) const DRAG_ACCENT: Color = Color::rgb(245, 196, 84);
/// The drag label's chip background.
pub(super) const DRAG_GHOST_BG: Color = Color::rgb(28, 24, 12);

/// One slot per [`Palette`] field, in declaration order. `xuid` is a single
/// task, so relaxed atomics are only there to keep the static safe.
static SLOTS: [AtomicU32; 16] = [const { AtomicU32::new(0) }; 16];

fn slots(p: &Palette) -> [u32; 16] {
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
        p.taskbar_entry,
        p.taskbar_entry_min,
        p.taskbar_entry_focus,
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
pub(super) fn taskbar_bg() -> Color {
    color(8)
}
pub(super) fn taskbar_entry() -> Color {
    color(9)
}
pub(super) fn taskbar_entry_min() -> Color {
    color(10)
}
pub(super) fn taskbar_entry_focus() -> Color {
    color(11)
}
pub(super) fn overlay_bg() -> Color {
    color(12)
}
pub(super) fn overlay_border() -> Color {
    color(13)
}
pub(super) fn overlay_selected() -> Color {
    color(14)
}
pub(super) fn overlay_text() -> Color {
    color(15)
}
