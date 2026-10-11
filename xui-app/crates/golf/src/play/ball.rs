//! The ball in the air: gravity, quadratic drag, Magnus lift from backspin
//! and the curve of sidespin, wind as a velocity of the air. A shot is
//! integrated once, at impact, into a list of samples that playback then
//! indexes by time; the camera director reads the list ahead of the ball
//! (docs/golf-gameplay-plan.md, 4.1).
//!
//! [`leg`] flies the ball until it meets the ground or something solid;
//! [`simulate`] is one leg (the carry), and `shot::play_out` strings legs
//! together with bounces and the roll.

use crate::math::{Vec2, Vec3};

/// The integration step in the air, seconds. Samples are kept at this rate.
pub const STEP: f32 = 1.0 / 240.0;
pub const GRAVITY: f32 = 9.81;
/// A leg never lasts longer than this, seconds (a stuck ball must end).
const MAX_LEG: f32 = 20.0;
const AIR_DENSITY: f32 = 1.2;
const DRAG_COEFFICIENT: f32 = 0.25;
pub const RADIUS: f32 = 0.02135;
const MASS: f32 = 0.0459;
/// Lift coefficient per unit of spin ratio, and its ceiling.
const LIFT_PER_SPIN: f32 = 2.0;
const LIFT_MAX: f32 = 0.30;
/// Spin lost per second (a ball keeps most of its spin over six seconds).
const SPIN_DECAY: f32 = 0.05;
pub const RPM: f32 = std::f32::consts::TAU / 60.0;

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
    /// First contact with the ground.
    Land,
    /// A later contact, and a hop.
    Bounce,
    /// The ball struck a tree and dropped.
    TreeHit,
    /// The ball came to rest. The last sample of a shot that ends so.
    Rest,
    /// The ball entered water. The last sample.
    Splash,
    /// The ball crossed out of bounds. The last sample.
    OutOfBounds,
    /// The ball dropped in the cup. The last sample.
    Holed,
}

impl Event {
    /// Whether the shot is over at this event.
    pub fn is_final(self) -> bool {
        matches!(
            self,
            Event::Rest | Event::Splash | Event::OutOfBounds | Event::Holed
        )
    }
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
        if t <= 0.0 || s.len() < 2 {
            return s[0].position;
        }
        if t >= last.t {
            return last.position;
        }
        let next = s.partition_point(|p| p.t <= t).clamp(1, s.len() - 1);
        let (a, b) = (&s[next - 1], &s[next]);
        let k = ((t - a.t) / (b.t - a.t).max(1e-6)).clamp(0.0, 1.0);
        a.position + (b.position - a.position) * k
    }

    /// The ball's velocity `t` seconds after impact (the nearest sample's).
    pub fn velocity_at(&self, t: f32) -> Vec3 {
        let s = &self.samples;
        let i = s.partition_point(|p| p.t <= t).saturating_sub(1);
        s.get(i).map_or(Vec3::default(), |p| p.velocity)
    }

    /// The first sample that carries `event`, if the flight has one.
    pub fn event(&self, event: Event) -> Option<&Sample> {
        self.samples.iter().find(|s| s.event == event)
    }

    /// How the shot ended (`Event::None` for a flight cut short).
    pub fn end(&self) -> Event {
        self.samples.last().map_or(Event::None, |s| s.event)
    }

    /// Ground distance from the launch to the first landing, metres.
    pub fn carry(&self) -> f32 {
        let land = self.event(Event::Land).or(self.samples.last());
        match (self.samples.first(), land) {
            (Some(a), Some(b)) => a.position.ground().distance(b.position.ground()),
            _ => 0.0,
        }
    }

    /// Ground distance from the launch to where the ball finished.
    pub fn total(&self) -> f32 {
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

/// The ball's state between legs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct State {
    pub position: Vec3,
    pub velocity: Vec3,
    /// Spin against the direction of travel, and about the vertical axis
    /// (positive: to the right), radians per second.
    pub backspin: f32,
    pub sidespin: f32,
    pub t: f32,
}

impl State {
    pub fn launched(l: &Launch) -> State {
        let h = l.heading.normalized();
        State {
            position: l.position,
            velocity: Vec3::new(
                h.x * l.angle.cos() * l.speed,
                l.angle.sin() * l.speed,
                h.y * l.angle.cos() * l.speed,
            ),
            backspin: l.backspin * RPM,
            sidespin: l.sidespin * RPM,
            t: 0.0,
        }
    }
}

/// What ended a leg.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Contact {
    /// The ball met the ground: `State` holds the point on the surface and
    /// the velocity it arrived with.
    Ground,
    /// The ball entered something solid (a tree).
    Solid,
    /// The leg ran out of time.
    Timeout,
}

/// Flies `state` through `wind` until it meets the ground (`ground(x, z)` is
/// the surface height) or `solid(p, t)` says it is inside something, pushing a
/// sample per step. A sample carries `Event::Apex` where the ball stops
/// rising; the caller labels the contact sample.
pub fn leg(
    state: &mut State,
    wind: Wind,
    ground: &dyn Fn(f32, f32) -> f32,
    solid: &dyn Fn(Vec3, f32) -> bool,
    out: &mut Vec<Sample>,
) -> Contact {
    // Backspin's axis is horizontal and across the heading (so that
    // spin x travel points up); sidespin's is vertical, and positive
    // sidespin pushes the ball to the right of the heading.
    let h = state.velocity.ground().normalized();
    let across = Vec3::new(h.x, 0.0, h.y).cross(Vec3::new(0.0, 1.0, 0.0));
    let area = std::f32::consts::PI * RADIUS * RADIUS;
    let drag_k = 0.5 * AIR_DENSITY * DRAG_COEFFICIENT * area / MASS;
    let lift_k = 0.5 * AIR_DENSITY * area / MASS;
    let decay = (-SPIN_DECAY * STEP).exp();
    let mut rising = state.velocity.y > 0.0;
    let start = state.t;
    while state.t - start < MAX_LEG {
        let relative = state.velocity - wind.0;
        let speed = relative.length().max(1e-3);
        let direction = relative * (1.0 / speed);
        let mut accel = Vec3::new(0.0, -GRAVITY, 0.0) - relative * (drag_k * speed);
        // Magnus: the spin vector crossed with the airflow's direction.
        let spin = across * state.backspin + Vec3::new(0.0, -state.sidespin, 0.0);
        let spin_speed = spin.length();
        if spin_speed > 1e-3 {
            let ratio = spin_speed * RADIUS / speed;
            let lift = (LIFT_PER_SPIN * ratio).min(LIFT_MAX);
            let force = spin.normalized().cross(direction);
            accel = accel + force * (lift_k * lift * speed * speed);
        }
        state.velocity = state.velocity + accel * STEP;
        let before = state.position;
        state.position = state.position + state.velocity * STEP;
        state.backspin *= decay;
        state.sidespin *= decay;
        state.t += STEP;

        let p = state.position;
        let surface = ground(p.x, p.z);
        if p.y <= surface {
            // Land where the step crossed the surface.
            let a = before.y - ground(before.x, before.z);
            let b = p.y - surface;
            let k = if a - b > 1e-6 {
                (a / (a - b)).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let at = before + (p - before) * k;
            state.position = Vec3::new(at.x, ground(at.x, at.z), at.z);
            state.t -= STEP * (1.0 - k);
            out.push(Sample {
                t: state.t,
                position: state.position,
                velocity: state.velocity,
                event: Event::None,
            });
            return Contact::Ground;
        }
        let event = if rising && state.velocity.y <= 0.0 {
            rising = false;
            Event::Apex
        } else {
            Event::None
        };
        out.push(Sample {
            t: state.t,
            position: state.position,
            velocity: state.velocity,
            event,
        });
        if solid(state.position, state.t) {
            return Contact::Solid;
        }
    }
    Contact::Timeout
}

/// Integrates `launch` through `wind` until the ball meets the ground
/// (`ground(x, z)` is the surface height): the carry, with no bounce.
pub fn simulate(launch: &Launch, wind: Wind, ground: impl Fn(f32, f32) -> f32) -> Flight {
    let mut state = State::launched(launch);
    let mut samples = vec![Sample {
        t: 0.0,
        position: state.position,
        velocity: state.velocity,
        event: Event::None,
    }];
    if leg(&mut state, wind, &ground, &|_, _| false, &mut samples) == Contact::Ground {
        if let Some(last) = samples.last_mut() {
            last.event = Event::Land;
        }
    }
    Flight { samples }
}
