//! Scrolling the view through the scrollback: which input moves it, by how
//! much, and how the offset follows new output. The offset counts lines back
//! from the bottom (0 is following the output); the engine owns the lines.

use xui_core::Key;

/// Lines a wheel notch moves the view.
pub const WHEEL_LINES: usize = 3;
/// `xui` delivers `WHEEL_DELTA` (120) per notch.
const NOTCH: i32 = 120;

/// A requested change of the view.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Move {
    /// Back (towards older lines) by this many lines.
    Back(usize),
    /// Forward (towards the bottom) by this many lines.
    Forward(usize),
    /// The oldest line.
    Top,
    /// The bottom: following the output again.
    Bottom,
}

/// The move a key makes, if it is a scroll key: Shift+PgUp/PgDn a page,
/// Shift+Home/End the top and the bottom. Anything else (Ctrl chords too)
/// belongs to the program in the terminal.
pub fn key_move(key: Key, shift: bool, ctrl: bool, page: usize) -> Option<Move> {
    if !shift || ctrl {
        return None;
    }
    match key {
        Key::PAGE_UP => Some(Move::Back(page)),
        Key::PAGE_DOWN => Some(Move::Forward(page)),
        Key::HOME => Some(Move::Top),
        Key::END => Some(Move::Bottom),
        _ => None,
    }
}

/// The notches of a wheel `delta` (positive rolls up): whole notches, and a
/// finer wheel's partial one still counts as one.
fn notches(delta: i16) -> i32 {
    let delta = i32::from(delta);
    match delta / NOTCH {
        0 => delta.signum(),
        whole => whole,
    }
}

/// The move a wheel roll makes: 3 lines a notch, a page with Shift. Rolling
/// up goes back in time.
pub fn wheel_move(delta: i16, shift: bool, page: usize) -> Option<Move> {
    let notches = notches(delta);
    let step = if shift { page.max(1) } else { WHEEL_LINES };
    let lines = step.saturating_mul(notches.unsigned_abs() as usize);
    match notches {
        0 => None,
        n if n > 0 => Some(Move::Back(lines)),
        _ => Some(Move::Forward(lines)),
    }
}

/// The offset after `mv`, kept within `0..=max` (the lines in the history).
pub fn apply(mv: Move, offset: usize, max: usize) -> usize {
    let next = match mv {
        Move::Back(lines) => offset.saturating_add(lines),
        Move::Forward(lines) => offset.saturating_sub(lines),
        Move::Top => max,
        Move::Bottom => 0,
    };
    next.min(max)
}

/// The offset after the output moved the screen's top from absolute line
/// `before` to `after`: a view that is scrolled back stays on the same text
/// (the offset grows by the lines added), while the bottom stays the bottom.
/// Text that was evicted (`max` shrank) is clamped to the oldest line.
pub fn anchored(offset: usize, before: u64, after: u64, max: usize) -> usize {
    if offset == 0 {
        return 0;
    }
    let added = usize::try_from(after.saturating_sub(before)).unwrap_or(usize::MAX);
    offset.saturating_add(added).min(max)
}

/// What the wheel sends a full-screen program that has no mouse mode (T2 adds
/// that): three Up or Down arrows a notch, so `less` and `vi` scroll.
pub fn wheel_arrows(delta: i16) -> Vec<u8> {
    let notches = notches(delta);
    let arrow: &[u8] = if notches > 0 { b"\x1b[A" } else { b"\x1b[B" };
    let count = WHEEL_LINES * notches.unsigned_abs() as usize;
    arrow.repeat(count.min(WHEEL_LINES * 16))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_keys_need_shift_and_no_ctrl() {
        assert_eq!(key_move(Key::PAGE_UP, true, false, 19), Some(Move::Back(19)));
        assert_eq!(key_move(Key::PAGE_DOWN, true, false, 19), Some(Move::Forward(19)));
        assert_eq!(key_move(Key::HOME, true, false, 19), Some(Move::Top));
        assert_eq!(key_move(Key::END, true, false, 19), Some(Move::Bottom));
        assert_eq!(key_move(Key::PAGE_UP, false, false, 19), None, "plain PgUp is the program's");
        assert_eq!(key_move(Key::PAGE_UP, true, true, 19), None);
        assert_eq!(key_move(Key::UP, true, false, 19), None);
    }

    #[test]
    fn a_notch_is_three_lines_and_shift_makes_it_a_page() {
        assert_eq!(wheel_move(120, false, 19), Some(Move::Back(3)));
        assert_eq!(wheel_move(-240, false, 19), Some(Move::Forward(6)));
        assert_eq!(wheel_move(120, true, 19), Some(Move::Back(19)));
        assert_eq!(wheel_move(30, false, 19), Some(Move::Back(3)), "a fine wheel still moves");
        assert_eq!(wheel_move(0, false, 19), None);
    }

    #[test]
    fn moves_stay_inside_the_history() {
        assert_eq!(apply(Move::Back(3), 0, 100), 3);
        assert_eq!(apply(Move::Back(500), 90, 100), 100);
        assert_eq!(apply(Move::Forward(5), 3, 100), 0);
        assert_eq!(apply(Move::Top, 0, 100), 100);
        assert_eq!(apply(Move::Bottom, 40, 100), 0);
        assert_eq!(apply(Move::Back(3), 0, 0), 0, "no history, no scrolling");
    }

    #[test]
    fn the_view_stays_on_its_text_as_history_grows() {
        assert_eq!(anchored(0, 100, 130, 130), 0, "the bottom follows the output");
        assert_eq!(anchored(10, 100, 103, 103), 13);
        assert_eq!(anchored(10, 100, 100, 100), 10, "no new lines, no move");
        assert_eq!(anchored(50, 100, 160, 55), 55, "clamped when the text was evicted");
        assert_eq!(anchored(5, 100, 100, 0), 0, "the history was cleared");
    }

    #[test]
    fn the_wheel_sends_arrows_to_a_full_screen_program() {
        assert_eq!(wheel_arrows(120), b"\x1b[A\x1b[A\x1b[A".to_vec());
        assert_eq!(wheel_arrows(-120), b"\x1b[B\x1b[B\x1b[B".to_vec());
        assert_eq!(wheel_arrows(-240).len(), 6 * 3);
        assert!(wheel_arrows(i16::MAX).len() <= 3 * 16 * 3, "a runaway wheel is bounded");
    }
}
