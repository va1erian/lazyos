//! xuid UI constants (issue #194 split): window/taskbar geometry and the
//! chrome colour palette, split out of `xuid.rs` unchanged.

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

pub(super) const BACKGROUND: Color = Color::rgb(18, 22, 36);
pub(super) const WINDOW_BG: Color = Color::rgb(30, 36, 54);
pub(super) const TITLE_BG: Color = Color::rgb(52, 60, 92);
pub(super) const TITLE_BG_FOCUS: Color = Color::rgb(44, 112, 74);
pub(super) const TITLE_TEXT: Color = Color::rgb(228, 232, 245);
pub(super) const BORDER_COLOR: Color = Color::rgb(92, 106, 152);
pub(super) const BORDER_COLOR_FOCUS: Color = Color::rgb(140, 220, 160);
pub(super) const EMPTY_BG: Color = Color::rgb(16, 18, 28);
pub(super) const TASKBAR_BG: Color = Color::rgb(24, 28, 44);
pub(super) const TASKBAR_ENTRY: Color = Color::rgb(52, 60, 92);
pub(super) const TASKBAR_ENTRY_MIN: Color = Color::rgb(38, 44, 66);
pub(super) const TASKBAR_ENTRY_FOCUS: Color = Color::rgb(44, 112, 74);
pub(super) const CLOSE_BG: Color = Color::rgb(198, 76, 76);
pub(super) const MINIMIZE_BG: Color = Color::rgb(208, 168, 88);
pub(super) const BUTTON_TEXT: Color = Color::rgb(24, 24, 32);
/// Drop-target frame and drag-label accent (issue #145).
pub(super) const DRAG_ACCENT: Color = Color::rgb(245, 196, 84);
/// The drag label's chip background.
pub(super) const DRAG_GHOST_BG: Color = Color::rgb(28, 24, 12);
/// Alt+Tab overlay panel background and border (issue #167).
pub(super) const OVERLAY_BG: Color = Color::rgb(20, 24, 38);
pub(super) const OVERLAY_BORDER: Color = Color::rgb(122, 138, 196);
/// Alt+Tab selected-entry highlight and its text.
pub(super) const OVERLAY_SELECTED: Color = Color::rgb(44, 112, 74);
pub(super) const OVERLAY_TEXT: Color = Color::rgb(220, 226, 240);
