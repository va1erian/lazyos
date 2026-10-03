//! xuid UI constants (issue #194 split): window geometry and the chrome
//! colour palette. The palette is live: `themefeed` resolves the
//! `sys/ui/*` settings (`libs/uitheme`) into it, and every colour below is an
//! accessor reading the current value.

use core::sync::atomic::{AtomicU32, Ordering};
use uitheme::{Mode, Palette};
use user::messenger::display::{self, Color};

/// The desktop's integer UI scale (docs/hidpi-plan.md): every metric below is
/// a design size times this, so the chrome is drawn natively at 2x on a
/// 2560x1440 screen. Set once, before anything is laid out.
static SCALE: AtomicU32 = AtomicU32::new(1);

/// The UI scale in effect (1 or 2).
pub(super) fn scale() -> i32 {
    SCALE.load(Ordering::Relaxed) as i32
}

/// Fix the UI scale for this compositor; the chrome faces follow it.
pub(super) fn set_scale(scale: u32) {
    let scale = scale.clamp(1, uitheme::MAX_SCALE);
    SCALE.store(scale, Ordering::Relaxed);
    display::set_face_scale(scale);
}

/// A design size in pixels at the current scale.
pub(super) fn px(design: i32) -> i32 {
    design * scale()
}

/// Title-bar height in pixels.
pub(super) fn title_h() -> i32 {
    22 * scale()
}
/// Window border thickness in pixels.
pub(super) fn border() -> i32 {
    2 * scale()
}
/// Where the first window's top-left sits.
pub(super) fn pad() -> i32 {
    48 * scale()
}
/// Gap between tiled windows (issue #250).
pub(super) fn window_gap() -> i32 {
    16 * scale()
}
/// Offset added per full grid of windows when placement must cascade (issue
/// #250).
pub(super) fn cascade_step() -> i32 {
    32 * scale()
}
/// The width of a cascaded window kept on screen: enough to show its title
/// bar and grab it, even when the window itself is past the right edge.
pub(super) fn cascade_visible_w() -> i32 {
    240 * scale()
}
/// The size of the rectangle a window without shell icon geometry
/// (`SetIconGeometry`) zooms to and from, at the screen's bottom-left.
pub(super) fn icon_w() -> i32 {
    48 * scale()
}
pub(super) fn icon_h() -> i32 {
    20 * scale()
}
/// Title-bar button size in pixels.
pub(super) fn button() -> i32 {
    16 * scale()
}
/// Gap between the two title-bar buttons.
pub(super) fn button_gap() -> i32 {
    2 * scale()
}
/// Distance from the button group to the window's right edge.
pub(super) fn button_margin() -> i32 {
    3 * scale()
}
/// Interactive-resize frame grip: how far inside (and outside) a window side a
/// pointer grabs that edge.
pub(super) fn resize_grip() -> i32 {
    4 * scale()
}
/// How far outside the window rectangle a pointer still counts as on its
/// frame; the outer half of the grip.
pub(super) fn resize_out() -> i32 {
    2 * scale()
}
/// Corner grip span: how far along a side a point still grabs the corner, so
/// diagonal resizing is easy to hit.
pub(super) fn corner_grip() -> i32 {
    14 * scale()
}
/// Smallest content width a resizable window may have, in pixels: enough for
/// the three title-bar buttons plus a little title.
pub(super) fn min_content_w() -> i32 {
    120 * scale()
}
/// Smallest content height a resizable window may have, in pixels.
pub(super) fn min_content_h() -> i32 {
    40 * scale()
}
/// How much of a window's title bar must stay on screen when it is moved off
/// an edge, so it can be grabbed again.
pub(super) fn title_reachable_w() -> i32 {
    64 * scale()
}
/// Two title-bar presses within this many PIT ticks (10 ms each) are a
/// double-click (500 ms).
pub(super) const DOUBLE_CLICK_TICKS: u64 = 50;
/// Pointer slop, in pixels, allowed between the two presses of a
/// double-click.
pub(super) fn double_click_slop() -> i32 {
    4 * scale()
}

/// Drop-target frame and drag-label accent (issue #145).
pub(super) const DRAG_ACCENT: Color = Color::rgb(245, 196, 84);
/// The drag label's chip background.
pub(super) const DRAG_GHOST_BG: Color = Color::rgb(28, 24, 12);

/// One slot per [`Palette`] field `xuid` uses, in declaration order (plus
/// the mode as a flag). `xuid` is a single task, so relaxed atomics are only
/// there to keep the static safe.
static SLOTS: [AtomicU32; 17] = [const { AtomicU32::new(0) }; 17];

fn slots(p: &Palette) -> [u32; 17] {
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
        p.title_text_focus,
        p.empty_text,
        p.accent,
        u32::from(p.mode == Mode::Light),
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

fn unpack(v: u32) -> Color {
    Color::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

fn color(index: usize) -> Color {
    unpack(SLOTS[index].load(Ordering::Relaxed))
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
/// Text on an inactive title bar.
pub(super) fn title_text() -> Color {
    color(4)
}
/// Text on the focused title bar.
pub(super) fn title_text_focus() -> Color {
    color(13)
}
/// Text on the empty-window placeholder.
pub(super) fn empty_text() -> Color {
    color(14)
}
/// The accent colour in effect.
pub(super) fn accent() -> Color {
    color(15)
}
/// The desktop preset in effect.
pub(super) fn mode() -> Mode {
    if SLOTS[16].load(Ordering::Relaxed) == 1 {
        Mode::Light
    } else {
        Mode::Dark
    }
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
