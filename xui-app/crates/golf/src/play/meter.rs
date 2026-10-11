//! The swing meter, three clicks as in the games it copies (docs/golf-
//! gameplay-plan.md, section 5):
//!
//! 1. the first click starts the swing: a cursor rises along the bar;
//! 2. the second sets the power where the cursor is (or at the top, if it
//!    gets there first);
//! 3. the cursor then falls back toward the start line. The third click is
//!    the accuracy: on the line is straight; early hooks, late slices.
//!
//! The cursor is in units of a full swing: the power the player aims for
//! is a tick on the bar (`Club::power_for`), and a click at the tick sends
//! the ball the distance on the aim marker.

/// Seconds for the cursor to rise from 0 to a full swing; it falls at the
/// same speed.
pub const RISE_TIME: f32 = 1.3;
/// How far past the start line the cursor may fall before the swing is
/// taken for the player, in units of a full swing.
pub const LATE_LIMIT: f32 = 0.18;
/// An accuracy click this close to the line is dead straight.
const SNAP: f32 = 0.025;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Meter {
    /// Waiting for the first click.
    Ready,
    /// The cursor is rising; `cursor` is its height.
    Power { cursor: f32 },
    /// Power is set; the cursor is falling.
    Accuracy { power: f32, cursor: f32 },
    /// The swing is made.
    Done { power: f32, error: f32 },
}

impl Meter {
    /// A click.
    pub fn click(&mut self) {
        *self = match *self {
            Meter::Ready => Meter::Power { cursor: 0.0 },
            Meter::Power { cursor } => Meter::Accuracy {
                power: cursor,
                cursor,
            },
            Meter::Accuracy { power, cursor } => Meter::Done {
                power,
                error: if cursor.abs() < SNAP { 0.0 } else { cursor },
            },
            done @ Meter::Done { .. } => done,
        };
    }

    /// `dt` seconds pass.
    pub fn tick(&mut self, dt: f32) {
        let step = dt / RISE_TIME;
        *self = match *self {
            Meter::Power { cursor } => {
                let cursor = cursor + step;
                if cursor >= 1.0 {
                    // At the top the power is locked, as if clicked.
                    Meter::Accuracy {
                        power: 1.0,
                        cursor: 1.0,
                    }
                } else {
                    Meter::Power { cursor }
                }
            }
            Meter::Accuracy { power, cursor } => {
                let cursor = cursor - step;
                if cursor <= -LATE_LIMIT {
                    // Too late: the swing happens, as late as it can be.
                    Meter::Done {
                        power,
                        error: -LATE_LIMIT,
                    }
                } else {
                    Meter::Accuracy { power, cursor }
                }
            }
            other => other,
        };
    }

    /// The cursor's height on the bar, for drawing (negative: past the
    /// start line).
    pub fn cursor(&self) -> f32 {
        match *self {
            Meter::Ready => 0.0,
            Meter::Power { cursor } | Meter::Accuracy { cursor, .. } => cursor,
            Meter::Done { error, .. } => error,
        }
    }

    /// The power once it is set.
    pub fn power(&self) -> Option<f32> {
        match *self {
            Meter::Accuracy { power, .. } | Meter::Done { power, .. } => Some(power),
            _ => None,
        }
    }

    /// The finished swing: power (a share of a full swing) and the accuracy
    /// error (positive: early, negative: late).
    pub fn result(&self) -> Option<(f32, f32)> {
        match *self {
            Meter::Done { power, error } => Some((power, error)),
            _ => None,
        }
    }
}

/// What an accuracy error does to the ball: how far off line it starts
/// (radians, positive to the right) and its sidespin (rpm, positive to
/// the right). An early click hooks, a late one slices.
pub fn miss(error: f32) -> (f32, f32) {
    (-error * 0.10, -error * 1800.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(clicks: &[(f32, bool)]) -> Meter {
        // (seconds to wait, click after waiting)
        let mut m = Meter::Ready;
        for &(wait, click) in clicks {
            let mut left = wait;
            while left > 0.0 {
                m.tick(0.005);
                left -= 0.005;
            }
            if click {
                m.click();
            }
        }
        m
    }

    #[test]
    fn three_clicks_make_a_swing() {
        // Start, half power at 0.65 s, then the cursor falls from 0.5 to
        // the line in 0.65 s: clicking then is straight.
        let m = run(&[(0.0, true), (0.65, true), (0.65, true)]);
        let (power, error) = m.result().expect("done");
        assert!((power - 0.5).abs() < 0.02, "{power}");
        assert!(error.abs() < 0.03, "{error}");
        assert_eq!(miss(0.0), (-0.0, 0.0));
    }

    #[test]
    fn early_hooks_and_late_slices() {
        let early = run(&[(0.0, true), (0.65, true), (0.2, true)]);
        let late = run(&[(0.0, true), (0.65, true), (0.9, true)]);
        let (_, e) = early.result().unwrap();
        let (_, l) = late.result().unwrap();
        assert!(e > 0.1 && l < -0.05, "{e} {l}");
        let (dir_e, spin_e) = miss(e);
        let (dir_l, spin_l) = miss(l);
        assert!(dir_e < 0.0 && spin_e < 0.0, "early goes left");
        assert!(dir_l > 0.0 && spin_l > 0.0, "late goes right");
    }

    #[test]
    fn a_swing_left_alone_finishes_late_and_full() {
        let m = run(&[(0.0, true), (6.0, false)]);
        let (power, error) = m.result().expect("it swings by itself");
        assert_eq!(power, 1.0, "the cursor reached the top");
        assert_eq!(error, -LATE_LIMIT);
    }

    #[test]
    fn clicking_after_done_changes_nothing() {
        let mut m = run(&[(0.0, true), (0.5, true), (0.5, true)]);
        let before = m;
        m.click();
        m.tick(1.0);
        assert_eq!(m, before);
        assert!(Meter::Ready.result().is_none());
        assert_eq!(Meter::Ready.power(), None);
    }
}
