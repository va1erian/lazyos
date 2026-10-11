//! The ball in the air: gravity, quadratic drag, Magnus lift from backspin
//! and the curve of sidespin, wind as a velocity of the air. A shot is
//! integrated once, at impact, into a list of samples that playback then
//! indexes by time; the camera director reads the list ahead of the ball
//! (docs/golf-gameplay-plan.md, 4.1).
//!
//! Only the carry is modelled so far: a flight ends where the ball first
//! touches the ground (bounce and roll are the surface table's job).

use crate::math::{Vec2, Vec3};

/// The integration step, seconds. Samples are kept at this rate.
pub const STEP: f32 = 1.0 / 240.0;
const GRAVITY: f32 = 9.81;
/// A flight never lasts longer than this, seconds (a stuck ball must end).
const MAX_FLIGHT: f32 = 20.0;
const AIR_DENSITY: f32 = 1.2;
const DRAG_COEFFICIENT: f32 = 0.25;
const RADIUS: f32 = 0.02135;
const MASS: f32 = 0.0459;
/// Lift coefficient per unit of spin ratio, and its ceiling.
const LIFT_PER_SPIN: f32 = 2.0;
const LIFT_MAX: f32 = 0.30;
/// Spin lost per second (a ball keeps most of its spin over six seconds).
const SPIN_DECAY: f32 = 0.05;
const RPM: f32 = std::f32::consts::TAU / 60.0;

/// Wind: the velocity of the air over the ground, metres per second.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Wind(pub Vec3);

impl Wind {
    /// `speed` m/s blowing *toward* ground direction `toward`.
    pub fn blowing(toward: Vec2, speed: f32) -> Wind {
        Wind(Vec3::new(toward.x * speed, 0.0, toward.y * speed))
    }
}

/// How the ball leaves the club face.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Launch {
    pub position: Vec3,
    /// Unit ground direction of the shot (x east, y = world z south).
    pub heading: Vec2,
    /// Ball speed, m/s.
    pub speed: f32,
    /// Launch angle above the ground plane, radians.
    pub angle: f32,
    /// Revolutions per minute against the direction of travel.
    pub backspin: f32,
    /// Revolutions per minute about the vertical axis: positive curves
    /// to the right (a slice), negative to the left (a hook).
    pub sidespin: f32,
}

/// Something the ball did at a sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    None,
    /// The highest point.
    Apex,
    /// First contact with the ground; the last sample of a flight.
    Land,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Seconds since impact.
    pub t: f32,
    pub position: Vec3,
    pub velocity: Vec3,
    pub event: Event,
}

/// A simulated shot.
#[derive(Clone, Debug, PartialEq)]
pub struct Flight {
    pub samples: Vec<Sample>,
}

impl Flight {
    pub fn duration(&self) -> f32 {
        self.samples.last().map_or(0.0, |s| s.t)
    }

    /// Where the ball is `t` seconds after impact (clamped to the flight).
    pub fn at(&self, t: f32) -> Vec3 {
        let s = &self.samples;
        let Some(last) = s.last() else {
            return Vec3::default();
        };
        if t <= 0.0 {
            return s[0].position;
        }
        if t >= last.t || s.len() < 2 {
            return last.position;
        }
        let i = ((t / STEP) as usize).min(s.len() - 2);
        let k = ((t - s[i].t) / (s[i + 1].t - s[i].t).max(1e-6)).clamp(0.0, 1.0);
        s[i].position + (s[i + 1].position - s[i].position) * k
    }

    /// The sample that carries `event`, if the flight has one.
    pub fn event(&self, event: Event) -> Option<&Sample> {
        self.samples.iter().find(|s| s.event == event)
    }

    /// Ground distance from the launch to the landing, metres.
    pub fn carry(&self) -> f32 {
        match (self.samples.first(), self.samples.last()) {
            (Some(a), Some(b)) => a.position.ground().distance(b.position.ground()),
            _ => 0.0,
        }
    }

    pub fn apex_height(&self) -> f32 {
        self.samples
            .iter()
            .map(|s| s.position.y)
            .fold(f32::MIN, f32::max)
    }
}

/// Integrates `launch` through `wind` until the ball meets the ground
/// (`ground(x, z)` is the surface height).
pub fn simulate(launch: &Launch, wind: Wind, ground: impl Fn(f32, f32) -> f32) -> Flight {
    let h = launch.heading.normalized();
    let mut velocity = Vec3::new(
        h.x * launch.angle.cos() * launch.speed,
        launch.angle.sin() * launch.speed,
        h.y * launch.angle.cos() * launch.speed,
    );
    let mut position = launch.position;
    // Backspin's axis is horizontal and across the heading (so that
    // spin x travel points up); sidespin's is vertical, and positive
    // sidespin pushes the ball to the right of the heading.
    let across = Vec3::new(h.x, 0.0, h.y).cross(Vec3::new(0.0, 1.0, 0.0));
    let mut back = launch.backspin * RPM;
    let mut side = launch.sidespin * RPM;
    let area = std::f32::consts::PI * RADIUS * RADIUS;
    let drag_k = 0.5 * AIR_DENSITY * DRAG_COEFFICIENT * area / MASS;
    let lift_k = 0.5 * AIR_DENSITY * area / MASS;

    let mut samples = vec![Sample {
        t: 0.0,
        position,
        velocity,
        event: Event::None,
    }];
    let mut t = 0.0;
    let mut rising = true;
    while t < MAX_FLIGHT {
        let relative = velocity - wind.0;
        let speed = relative.length().max(1e-3);
        let direction = relative * (1.0 / speed);
        let mut accel = Vec3::new(0.0, -GRAVITY, 0.0) - relative * (drag_k * speed);
        // Magnus: the spin vector crossed with the airflow's direction.
        let spin = across * back + Vec3::new(0.0, -side, 0.0);
        let spin_speed = spin.length();
        if spin_speed > 1e-3 {
            let ratio = spin_speed * RADIUS / speed;
            let lift = (LIFT_PER_SPIN * ratio).min(LIFT_MAX);
            let force = spin.normalized().cross(direction);
            accel = accel + force * (lift_k * lift * speed * speed);
        }
        velocity = velocity + accel * STEP;
        let before = position;
        position = position + velocity * STEP;
        let decay = (-SPIN_DECAY * STEP).exp();
        (back, side) = (back * decay, side * decay);
        t += STEP;

        let surface = ground(position.x, position.z);
        if position.y <= surface {
            // Land where the step crossed the surface.
            let a = before.y - ground(before.x, before.z);
            let b = position.y - surface;
            let k = if a - b > 1e-6 {
                (a / (a - b)).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let at = before + (position - before) * k;
            samples.push(Sample {
                t: t - STEP * (1.0 - k),
                position: Vec3::new(at.x, ground(at.x, at.z), at.z),
                velocity,
                event: Event::Land,
            });
            return Flight { samples };
        }
        let event = if rising && velocity.y <= 0.0 {
            rising = false;
            Event::Apex
        } else {
            Event::None
        };
        samples.push(Sample {
            t,
            position,
            velocity,
            event,
        });
    }
    Flight { samples }
}
