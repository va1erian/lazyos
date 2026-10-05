//! The compact/expanded toggle the dashboards (`sysmon`, `fabricmon`) share.
//!
//! A dashboard is compact when its window is small. The mode is derived from
//! the window size alone ([`is_compact`]), so dragging the window small or
//! pressing `c` ends in the same state, and the compositor's `Configure` is
//! the only thing the app has to follow. The toggle asks the compositor for a
//! size (`RequestSize`) instead of flipping a flag: [`toggle_target`] picks
//! which.

/// The content size a compact dashboard asks the compositor for.
pub const COMPACT_SIZE: (u32, u32) = (320, 150);
/// The smallest content size a dashboard declares in its size hints; small
/// enough for [`COMPACT_SIZE`] and a little below it.
pub const MIN_SIZE: (u32, u32) = (220, 110);
/// Below this width the full layout no longer fits its columns.
const COMPACT_BELOW_W: i32 = 480;
/// Below this height the full layout no longer fits its sections.
const COMPACT_BELOW_H: i32 = 300;

/// Whether a `width` x `height` client area is too small for the full view.
pub fn is_compact(width: i32, height: i32) -> bool {
    width < COMPACT_BELOW_W || height < COMPACT_BELOW_H
}

/// The content size to request when the user toggles: the remembered full
/// size from a compact view, [`COMPACT_SIZE`] from a full one. A remembered
/// size that is itself compact (the app was opened small) falls back to
/// `fallback`, so the toggle can never request the size it is already at.
pub fn toggle_target(
    current: (i32, i32),
    remembered: (i32, i32),
    fallback: (u32, u32),
) -> (u32, u32) {
    if !is_compact(current.0, current.1) {
        return COMPACT_SIZE;
    }
    let size = if is_compact(remembered.0, remembered.1) {
        (fallback.0 as i32, fallback.1 as i32)
    } else {
        remembered
    };
    (size.0.max(0) as u32, size.1.max(0) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_windows_are_compact_and_large_ones_are_not() {
        assert!(is_compact(320, 150));
        assert!(is_compact(860, 200), "a short window is compact");
        assert!(is_compact(300, 600), "a narrow window is compact");
        assert!(!is_compact(860, 600));
        assert!(!is_compact(480, 300), "the thresholds are exclusive");
        assert!(is_compact(479, 300));
        assert!(is_compact(480, 299));
    }

    #[test]
    fn the_compact_size_is_compact_and_inside_the_hints() {
        assert!(is_compact(COMPACT_SIZE.0 as i32, COMPACT_SIZE.1 as i32));
        assert!(COMPACT_SIZE.0 >= MIN_SIZE.0 && COMPACT_SIZE.1 >= MIN_SIZE.1);
    }

    #[test]
    fn toggling_a_full_window_requests_the_compact_size() {
        assert_eq!(
            toggle_target((860, 600), (860, 600), (860, 600)),
            COMPACT_SIZE
        );
    }

    #[test]
    fn toggling_a_compact_window_restores_the_remembered_size() {
        assert_eq!(
            toggle_target((320, 150), (700, 500), (860, 600)),
            (700, 500)
        );
    }

    #[test]
    fn a_compact_remembered_size_falls_back_to_the_default() {
        assert_eq!(
            toggle_target((320, 150), (320, 150), (860, 600)),
            (860, 600)
        );
    }
}
