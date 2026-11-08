//! Triple-click detection. The backend turns the second quick press into a
//! `MouseDoubleClick` and then forgets the pair, so the third press arrives
//! as an ordinary press: this remembers the double-click to recognise it.

/// How long after a double-click a press still counts as the third click
/// (the backend's own double-click interval).
const WINDOW_NS: u64 = 500_000_000;
/// How far (pixels, per axis) the third press may land from the double-click.
const DISTANCE: i32 = 4;

#[derive(Default)]
pub struct Clicks {
    double: Option<(u64, i32, i32)>,
}

impl Clicks {
    /// A double-click happened at (`x`, `y`) at time `now` (nanoseconds).
    pub fn double(&mut self, now: u64, x: i32, y: i32) {
        self.double = Some((now, x, y));
    }

    /// Whether a plain press at (`x`, `y`) is the third click of a sequence.
    /// A triple-click consumes the sequence, so a fourth press starts over.
    pub fn is_triple(&mut self, now: u64, x: i32, y: i32) -> bool {
        let triple = self.double.is_some_and(|(at, dx, dy)| {
            now.saturating_sub(at) <= WINDOW_NS && (x - dx).abs() <= DISTANCE && (y - dy).abs() <= DISTANCE
        });
        self.double = None;
        triple
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    #[test]
    fn a_quick_press_in_place_after_a_double_click_is_the_third() {
        let mut clicks = Clicks::default();
        clicks.double(1000 * MS, 50, 60);
        assert!(clicks.is_triple(1300 * MS, 52, 58));
        assert!(!clicks.is_triple(1400 * MS, 52, 58), "consumed");
    }

    #[test]
    fn a_slow_or_distant_press_is_a_new_first_click() {
        let mut clicks = Clicks::default();
        clicks.double(1000 * MS, 50, 60);
        assert!(!clicks.is_triple(1600 * MS, 50, 60), "too late");
        clicks.double(2000 * MS, 50, 60);
        assert!(!clicks.is_triple(2100 * MS, 80, 60), "too far");
        assert!(!clicks.is_triple(2200 * MS, 50, 60), "a failed try also clears it");
    }

    #[test]
    fn a_press_with_no_double_click_is_never_a_triple() {
        assert!(!Clicks::default().is_triple(5, 0, 0));
    }
}
