//! Motion for the desktop: easing curves, a damped spring and tweens, all
//! pure functions of elapsed time (docs/notify-plan.md, "The cute part").
//!
//! * [`Curve`] maps progress `0..=1` to a position fraction (it may overshoot
//!   1 for a spring); [`Spring`] is the closed-form underdamped spring.
//! * [`Tween`] moves an integer from one value to another over a duration on
//!   a curve, can be re-aimed mid-flight without a jump ([`Tween::retarget`]),
//!   and says whether it is still moving so a caller redraws only then.
//! * [`Motion`] is the user's `sys/ui/motion` setting: `full`, `reduced`
//!   (short, non-moving) or `none` (instant). [`Motion::tween`] applies it, so
//!   a presenter never asks "is motion on" in its own code.
//!
//! Time is whole milliseconds the caller supplies; nothing here reads a
//! clock.

#![forbid(unsafe_code)]

/// The `sys/ui/motion` setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Motion {
    /// Springs, slides and flourishes as designed.
    #[default]
    Full,
    /// Nothing travels: a change is a short fade in place.
    Reduced,
    /// Every change is instant, with the same final layout.
    None,
}

impl Motion {
    /// The length of a fade under [`Motion::Reduced`].
    pub const REDUCED_MS: u32 = 120;

    /// The setting's value; anything unknown is [`Motion::Full`] (the
    /// default), so a typo never switches the desktop's animation off.
    pub fn parse(value: &str) -> Motion {
        match value.trim() {
            "reduced" => Motion::Reduced,
            "none" => Motion::None,
            _ => Motion::Full,
        }
    }

    /// The setting's value.
    pub const fn as_str(self) -> &'static str {
        match self {
            Motion::Full => "full",
            Motion::Reduced => "reduced",
            Motion::None => "none",
        }
    }

    /// Whether things travel across the screen.
    pub const fn travels(self) -> bool {
        matches!(self, Motion::Full)
    }

    /// A tween from `from` to `to` starting at `now`, as this setting allows:
    /// the given curve and duration under `Full`; under `Reduced` a short
    /// linear one (for a fade, not a slide: the caller draws it as one); and
    /// under `None` one already finished.
    pub fn tween(self, from: i32, to: i32, now: u64, duration_ms: u32, curve: Curve) -> Tween {
        match self {
            Motion::Full => Tween::new(from, to, now, duration_ms, curve),
            Motion::Reduced => Tween::new(
                from,
                to,
                now,
                duration_ms.min(Motion::REDUCED_MS),
                Curve::Linear,
            ),
            Motion::None => Tween::new(from, to, now, 0, Curve::Linear),
        }
    }
}

/// A curve from progress to position fraction. Every curve maps 0 to 0 and 1
/// to (about) 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Curve {
    Linear,
    /// Fast start, gentle stop (a slide out, a reflow).
    EaseOut,
    /// Gentle start and stop.
    EaseInOut,
    /// A damped spring; the progress is the fraction of its settle time.
    Spring(Spring),
}

impl Curve {
    /// The position fraction at progress `t` (clamped to `0..=1`).
    pub fn at(self, t: f32) -> f32 {
        let t = clamp01(t);
        match self {
            Curve::Linear => t,
            Curve::EaseOut => ease_out_cubic(t),
            Curve::EaseInOut => ease_in_out_cubic(t),
            // The progress spans the settle time, where the spring is within
            // 1% of its target; the tween then snaps to the target.
            Curve::Spring(spring) => spring.at(t * spring.settle_ms() as f32),
        }
    }
}

/// `t` limited to `0..=1` (NaN is 0).
pub fn clamp01(t: f32) -> f32 {
    if t.is_nan() {
        0.0
    } else {
        t.clamp(0.0, 1.0)
    }
}

/// Cubic ease-out: `1 - (1 - t)^3`.
pub fn ease_out_cubic(t: f32) -> f32 {
    let u = 1.0 - clamp01(t);
    1.0 - u * u * u
}

/// Cubic ease-in-out.
pub fn ease_in_out_cubic(t: f32) -> f32 {
    let t = clamp01(t);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        let u = -2.0 * t + 2.0;
        1.0 - u * u * u / 2.0
    }
}

/// The closed-form underdamped spring `x(t)` that starts at 0 at rest and
/// settles at 1, overshooting on the way.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    /// Natural angular frequency, radians per millisecond.
    pub omega: f32,
    /// Damping ratio; below 1 it overshoots.
    pub zeta: f32,
}

impl Spring {
    /// The toast's slide: about 7% overshoot, settled in ~300 ms.
    pub const TOAST: Spring = Spring {
        omega: 0.02,
        zeta: 0.65,
    };

    fn ratio(self) -> f32 {
        self.zeta.clamp(0.05, 0.999)
    }

    /// The position fraction `t_ms` milliseconds after the start.
    pub fn at(self, t_ms: f32) -> f32 {
        if t_ms <= 0.0 {
            return 0.0;
        }
        let zeta = self.ratio();
        let damped = self.omega * (1.0 - zeta * zeta).sqrt();
        let decay = (-zeta * self.omega * t_ms).exp();
        let wave = (damped * t_ms).cos() + zeta * self.omega / damped * (damped * t_ms).sin();
        1.0 - decay * wave
    }

    /// A time after which the spring stays within 1% of its target (from
    /// its decay envelope), milliseconds.
    pub fn settle_ms(self) -> u32 {
        let zeta = self.ratio();
        // The envelope e^(-zeta omega t) / sqrt(1 - zeta^2) falls under 1%.
        let envelope = 0.01 * (1.0 - zeta * zeta).sqrt();
        (-envelope.ln() / (zeta * self.omega)).ceil() as u32
    }

    /// The largest value the spring reaches (the overshoot peak).
    pub fn peak(self) -> f32 {
        let zeta = self.ratio();
        1.0 + (-core::f32::consts::PI * zeta / (1.0 - zeta * zeta).sqrt()).exp()
    }
}

/// An integer value moving from one value to another.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tween {
    from: i32,
    to: i32,
    start: u64,
    duration: u32,
    curve: Curve,
}

impl Tween {
    /// A tween starting at `now` (milliseconds); a zero duration is already
    /// at `to`.
    pub const fn new(from: i32, to: i32, now: u64, duration_ms: u32, curve: Curve) -> Tween {
        Tween {
            from,
            to,
            start: now,
            duration: duration_ms,
            curve,
        }
    }

    /// A tween that rests at `value`.
    pub const fn at_rest(value: i32) -> Tween {
        Tween::new(value, value, 0, 0, Curve::Linear)
    }

    /// The value at `now`; before the start it is `from`, after the end `to`.
    pub fn value(&self, now: u64) -> i32 {
        if self.done(now) {
            return self.to;
        }
        let elapsed = now.saturating_sub(self.start) as f32;
        let fraction = self.curve.at(elapsed / self.duration as f32);
        let span = (self.to - self.from) as f32;
        self.from + (span * fraction).round() as i32
    }

    /// Whether the value has reached `to` for good by `now`.
    pub fn done(&self, now: u64) -> bool {
        self.duration == 0 || now.saturating_sub(self.start) >= u64::from(self.duration)
    }

    /// The target.
    pub const fn target(&self) -> i32 {
        self.to
    }

    /// Aim at `to` from wherever the value is at `now`, over `duration_ms`
    /// on `curve`: no jump when a toast is moved again mid-slide. Aiming at
    /// the current target keeps the tween as it is.
    pub fn retarget(&mut self, now: u64, to: i32, duration_ms: u32, curve: Curve) {
        if to == self.to {
            return;
        }
        *self = Tween::new(self.value(now), to, now, duration_ms, curve);
    }

    /// The milliseconds left at `now` (0 when done).
    pub fn remaining(&self, now: u64) -> u32 {
        if self.done(now) {
            0
        } else {
            (u64::from(self.duration) - now.saturating_sub(self.start)) as u32
        }
    }
}

#[cfg(test)]
mod tests;
