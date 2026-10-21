//! The wheel, as webd takes it: whole notches, at most [`WHEEL_NOTCHES_MAX`] per `Wheel` message.

/// xui's wheel unit: 120 for one notch.
const NOTCH: i32 = 120;
/// Most notches one `Wheel` message carries: the engine takes at most this many per call.
pub(super) const WHEEL_NOTCHES_MAX: i32 = 8;

/// Whole notches of a wheel `delta`; a small (smooth) delta still moves once.
pub(super) fn notches(delta: i16) -> i32 {
    let delta = i32::from(delta);
    match delta / NOTCH {
        0 => delta.signum(),
        whole => whole,
    }
}

/// The `Wheel` messages for `across` and `down` notches: as many as it takes at most
/// [`WHEEL_NOTCHES_MAX`] notches each, so the page moves by the whole sum.
pub(super) fn wheel_steps(mut across: i32, mut down: i32) -> Vec<(i32, i32)> {
    let mut steps = Vec::new();
    while across != 0 || down != 0 {
        let step_x = across.clamp(-WHEEL_NOTCHES_MAX, WHEEL_NOTCHES_MAX);
        let step_y = down.clamp(-WHEEL_NOTCHES_MAX, WHEEL_NOTCHES_MAX);
        steps.push((step_x, step_y));
        across -= step_x;
        down -= step_y;
    }
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_of_notches_is_sent_whole_in_chunks_the_engine_takes() {
        assert_eq!(wheel_steps(0, 0), vec![]);
        assert_eq!(wheel_steps(0, -5), vec![(0, -5)]);
        assert_eq!(wheel_steps(0, 20), vec![(0, 8), (0, 8), (0, 4)]);
        assert_eq!(wheel_steps(-9, 3), vec![(-8, 3), (-1, 0)]);
        let total: i32 = wheel_steps(0, -37).iter().map(|(_, y)| y).sum();
        assert_eq!(total, -37);
    }

    #[test]
    fn the_wheel_moves_at_least_one_notch_and_the_right_way() {
        assert_eq!(notches(120), 1);
        assert_eq!(notches(-240), -2);
        assert_eq!(notches(30), 1);
        assert_eq!(notches(-30), -1);
        assert_eq!(notches(0), 0);
    }
}
