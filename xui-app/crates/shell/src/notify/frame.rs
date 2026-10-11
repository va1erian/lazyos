//! What the presenter draws for one toast at one instant: where it is, how
//! far into its animations it is, and the pure curves of the level icon's
//! arrival flourish (docs/notify-plan.md, "The cute part").
//!
//! Everything here is a function of milliseconds since the toast appeared:
//! no clock, no state, so a test pins the curves and the presenter redraws
//! only while one is moving.

use lazymotion::{ease_out_cubic, Motion};

use super::note::Level;
use crate::Rect;

/// Where a toast is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// Sliding in (and settling).
    Entering,
    /// At rest.
    Shown,
    /// Sinking out; the window goes when it is done.
    Leaving,
}

/// The arrival flourish lasts this long, after [`FLOURISH_DELAY_MS`].
pub const FLOURISH_MS: u32 = 600;
/// The flourish starts once the slide is under way.
pub const FLOURISH_DELAY_MS: u32 = 90;

/// An icon's offset from its place, in design pixels, and how much of the
/// Success tick is drawn (`0..=1`; 1 for every other level).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Flourish {
    pub dx: f32,
    pub dy: f32,
    pub tick: f32,
}

impl Flourish {
    /// The icon at rest.
    pub const REST: Flourish = Flourish {
        dx: 0.0,
        dy: 0.0,
        tick: 1.0,
    };
}

/// The flourish of a `level` icon `age_ms` after the toast appeared: a
/// gentle wiggle for Info, a tick that draws itself for Success, a nudge up
/// for Warn and a short shake for Error. All end at [`Flourish::REST`].
pub fn flourish(level: Level, age_ms: u32) -> Flourish {
    let Some(t_ms) = age_ms.checked_sub(FLOURISH_DELAY_MS) else {
        // Before it starts: a Success tick is not drawn yet.
        return Flourish {
            tick: if level == Level::Success { 0.0 } else { 1.0 },
            ..Flourish::REST
        };
    };
    let t = t_ms as f32 / 1000.0;
    let span = FLOURISH_MS as f32 / 1000.0;
    if t >= span {
        return Flourish::REST;
    }
    let left = 1.0 - t / span;
    let turn = core::f32::consts::TAU;
    match level {
        Level::Info => Flourish {
            dx: 2.0 * (turn * 4.0 * t).sin() * left,
            ..Flourish::REST
        },
        Level::Success => Flourish {
            // The tick takes the first 400 ms, then holds.
            tick: ease_out_cubic(t / 0.4),
            ..Flourish::REST
        },
        Level::Warn => Flourish {
            dy: -3.0 * (turn * 1.7 * t).sin().abs() * left,
            ..Flourish::REST
        },
        Level::Error => Flourish {
            dx: 3.0 * (turn * 7.0 * t).sin() * left * left,
            ..Flourish::REST
        },
    }
}

/// The quantised parts of a [`Frame`] that change what is painted. A window
/// is redrawn only when its look changes; moving it is not a redraw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    /// The flourish position, in 33 ms steps, `None` once it is over.
    pub flourish: Option<u32>,
    /// Content opacity in sixteenths (reduced motion fades in place).
    pub fade: u8,
    /// The progress bar's fill in thousandths.
    pub progress: Option<u16>,
    /// The countdown bar's remaining length in hundredths (while hovered).
    pub countdown: Option<u8>,
    pub hovered: bool,
}

/// One toast as it is to be drawn now.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub id: u64,
    /// Whole-surface rectangle in design pixels (the slide's position).
    pub rect: Rect,
    pub stage: Stage,
    /// Milliseconds since it appeared.
    pub age_ms: u32,
    /// Content opacity `0..=1`: 1 unless motion is reduced.
    pub fade: f32,
    /// The progress bar's fill `0..=1`, eased between updates.
    pub progress: Option<f32>,
    /// Time left of its life as a fraction, shown while the pointer rests
    /// on it (`None` for a toast that does not time out).
    pub countdown: Option<f32>,
    pub hovered: bool,
    /// Whether the slide is still moving: the window needs placing.
    pub moving: bool,
}

impl Frame {
    /// What this frame draws, quantised.
    pub fn look(&self) -> Look {
        let end = FLOURISH_DELAY_MS + FLOURISH_MS;
        Look {
            flourish: (self.age_ms < end).then_some(self.age_ms / 33),
            fade: (self.fade * 16.0).round() as u8,
            progress: self.progress.map(|p| (p * 1000.0).round() as u16),
            countdown: self.countdown.map(|c| (c * 100.0).round() as u8),
            hovered: self.hovered,
        }
    }

    /// The icon's flourish, with motion set to `motion`: only `Full`
    /// flourishes.
    pub fn flourish(&self, level: Level, motion: Motion) -> Flourish {
        if motion == Motion::Full && self.stage != Stage::Leaving {
            flourish(level, self.age_ms)
        } else {
            Flourish::REST
        }
    }
}
