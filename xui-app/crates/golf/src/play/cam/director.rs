//! The flight cameras (docs/golf-gameplay-plan.md, 4.2 and 4.4). A shot is
//! simulated before it is shown, so the director plans its cuts with the
//! whole flight in hand: it tries a ring of camera spots around the
//! landing and keeps the one from which the ball stays at a watchable
//! distance and a watchable angular speed from the apex to the rest.
//!
//! Until the first cut the camera is the address camera, held in place and
//! panning after the ball with its zoom narrowing as the ball goes away.

use crate::math::{Vec2, Vec3};
use crate::render::Camera;

use super::obstacles::Obstacles;
use crate::play::ball::{Event, Flight};

/// A flight shorter than this (to its landing) is only tracked: the
/// address camera sees all of it.
const CUT_MIN_CARRY: f32 = 60.0;
/// The ball is never closer than this to a landing camera, metres, nor
/// further than [`MAX_RANGE`].
const MIN_RANGE: f32 = 14.0;
const MAX_RANGE: f32 = 260.0;
/// The fastest the line of sight may turn, radians per second.
const MAX_RATE: f32 = 0.6;
/// Seconds between the samples a spot is judged on.
const JUDGE_STEP: f32 = 0.05;
/// How quickly the pan catches up with the ball: seconds.
const PAN_TAU: f32 = 0.10;
/// Eye height over the ground at a landing camera, metres, and the
/// higher spot tried when a rise hides the ball from the low one.
const LANDING_HEIGHT: f32 = 3.0;
const HIGH_HEIGHT: f32 = 9.0;
/// The zoom narrows the view from this range on, down to this share of
/// the cut's field of view: enough to keep the ball a size worth watching
/// without losing the ground it is going to.
const REFERENCE_RANGE: f32 = 90.0;
const MIN_ZOOM: f32 = 0.6;
/// Seconds before the ball drops that the cup camera cuts in.
const CUP_LEAD: f32 = 1.0;

/// A planned cut: at `t` seconds the camera is at `eye` with `fov`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cut {
    pub t: f32,
    pub eye: Vec3,
    /// Horizontal field of view when the ball is near, radians.
    pub fov: f32,
}

/// The flight camera: an eye, a pan and a zoom following a ball.
#[derive(Clone, Debug, PartialEq)]
pub struct Director {
    pub camera: Camera,
    cuts: Vec<Cut>,
    next: usize,
    /// The field of view at the current cut; the zoom narrows from it.
    base_fov: f32,
    /// Where the shot comes down: kept in the picture with the ball.
    focus: Option<Vec3>,
    /// Height over width of the picture.
    aspect: f32,
}

impl Director {
    /// A director for `flight`, starting from the address camera.
    pub fn new(
        address: &Camera,
        flight: &Flight,
        world: &Obstacles,
        pin: Option<Vec2>,
        aspect: f32,
    ) -> Director {
        let cuts = plan(flight, world, address.eye, pin);
        Director {
            camera: *address,
            cuts,
            next: 0,
            base_fov: address.fov,
            focus: flight.event(Event::Land).map(|s| s.position),
            aspect,
        }
    }

    /// The cuts planned, for tests.
    pub fn cuts(&self) -> &[Cut] {
        &self.cuts
    }

    /// `dt` seconds pass; the flight is at `t` with the ball at `ball`
    /// moving at `velocity`.
    pub fn update(&mut self, dt: f32, t: f32, ball: Vec3, velocity: Vec3) {
        let mut cut = false;
        while self.cuts.get(self.next).is_some_and(|c| c.t <= t) {
            let c = self.cuts[self.next];
            self.camera.eye = c.eye;
            self.base_fov = c.fov;
            self.next += 1;
            cut = true;
        }
        // Aim a little ahead of the ball, so the pan leads.
        let target = ball + velocity * 0.12;
        let to = target - self.camera.eye;
        let yaw = to.x.atan2(-to.z);
        let up = |p: Vec3| {
            let d = p - self.camera.eye;
            d.y.atan2(Vec2::new(d.x, d.z).length().max(0.1))
        };
        let toward_ball = up(target);
        // Half the picture's height as an angle, with the zoom as it will be.
        let distance = (ball - self.camera.eye).length().max(1.0);
        let zoom = (REFERENCE_RANGE / distance).clamp(MIN_ZOOM, 1.0);
        let want = self.base_fov * zoom;
        let half = ((want * 0.5).tan() * self.aspect).atan();
        // Frame the ball and where it comes down together when both fit:
        // half-way between them, but the ball stays in the middle part.
        let mut pitch = toward_ball;
        if let Some(focus) = self.focus {
            let mid = 0.5 * (toward_ball + up(focus));
            pitch = mid.clamp(toward_ball - 0.6 * half, toward_ball + 0.6 * half);
        }
        let k = if cut {
            1.0
        } else {
            1.0 - (-dt / PAN_TAU).exp()
        };
        self.camera.yaw += wrap(yaw - self.camera.yaw) * k;
        self.camera.pitch += (pitch - self.camera.pitch) * k;
        let kf = if cut { 1.0 } else { 1.0 - (-dt / 0.3).exp() };
        self.camera.fov += (want - self.camera.fov) * kf;
    }
}

fn wrap(a: f32) -> f32 {
    (a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI
}

/// The cuts for `flight`, in time order.
pub fn plan(flight: &Flight, world: &Obstacles, start_eye: Vec3, pin: Option<Vec2>) -> Vec<Cut> {
    let mut cuts = Vec::new();
    let end_t = flight.duration();
    let (apex, land) = (flight.event(Event::Apex), flight.event(Event::Land));
    let begin = flight
        .samples
        .first()
        .map_or(Vec2::default(), |s| s.position.ground());

    let mut last_cut = 0.0;
    if let (Some(apex), Some(land)) = (apex, land) {
        let landing = land.position;
        if landing.ground().distance(begin) >= CUT_MIN_CARRY && end_t - apex.t > 1.0 {
            let near_pin = pin.is_some_and(|p| p.distance(landing.ground()) < 25.0);
            if let Some(eye) = landing_spot(flight, world, apex.t, begin, near_pin, start_eye) {
                cuts.push(Cut {
                    t: apex.t,
                    eye,
                    fov: 50f32.to_radians(),
                });
                last_cut = apex.t;
            }
        }
    }
    // The cup: close and low, from the side, as the ball drops.
    if flight.end() == Event::Holed && end_t > 2.0 {
        if let (Some(pin), Some(prev)) = (
            pin,
            flight.samples.get(flight.samples.len().saturating_sub(2)),
        ) {
            let came = (pin - prev.position.ground()).normalized();
            let at = pin + came.perp() * 2.2 - came * 0.5;
            let ground = world.ground(at.x, at.y);
            let t = (end_t - CUP_LEAD).max(last_cut + 0.5);
            cuts.push(Cut {
                t,
                eye: Vec3::new(at.x, ground + 0.5, at.y),
                fov: 35f32.to_radians(),
            });
        }
    }
    cuts
}

/// The best spot to watch the rest of the flight from `from_t`, or `None`
/// when no spot is clear of the ground and the trees.
fn landing_spot(
    flight: &Flight,
    world: &Obstacles,
    from_t: f32,
    begin: Vec2,
    near_pin: bool,
    start_eye: Vec3,
) -> Option<Vec3> {
    let landing = flight.event(Event::Land)?.position;
    let dir = (landing.ground() - begin).normalized();
    let side = dir.perp();
    let path: Vec<(f32, Vec3)> = {
        let mut v = Vec::new();
        let mut t = from_t;
        while t < flight.duration() {
            v.push((t, flight.at(t)));
            t += JUDGE_STEP;
        }
        v.push((flight.duration(), flight.at(flight.duration())));
        v
    };
    let mut best: Option<(f32, Vec3)> = None;
    for height in [LANDING_HEIGHT, HIGH_HEIGHT] {
        for along in [-45.0f32, -30.0, 35.0, 55.0, 80.0] {
            for across in [-15.0f32, 15.0, -28.0, 28.0] {
                let p = landing.ground() + dir * along + side * across;
                let ground = world.ground(p.x, p.y);
                let eye = Vec3::new(p.x, ground + height, p.y);
                if world.inside_tree(eye) {
                    continue;
                }
                let (min_d, max_d, rate) = judge(&path, eye);
                // How much of the rest of the flight is hidden by a rise or
                // a tree: the dearest fault, since a ball nobody can see
                // is no shot to watch.
                let hidden = path
                    .iter()
                    .step_by(4)
                    .filter(|(_, p)| world.blocked(eye, *p, 0.5))
                    .count() as f32
                    / path.len().div_ceil(4).max(1) as f32;
                let mut cost = rate
                    + (MIN_RANGE - min_d).max(0.0) * 0.1
                    + (max_d - MAX_RANGE).max(0.0) * 0.01
                    + hidden * 8.0
                    + (height - LANDING_HEIGHT) * 0.02;
                // Look back from past a green near the pin; stay near where
                // the shot began.
                if near_pin && along > 0.0 {
                    cost -= 0.15;
                }
                cost += eye.ground().distance(start_eye.ground()) * 0.0002;
                if best.is_none_or(|(c, _)| cost < c) {
                    best = Some((cost, eye));
                }
            }
        }
    }
    best.map(|(_, eye)| eye)
}

/// For a ball moving along `path` seen from `eye`: the nearest and
/// furthest it gets, and the fastest the line of sight turns.
fn judge(path: &[(f32, Vec3)], eye: Vec3) -> (f32, f32, f32) {
    let (mut min_d, mut max_d, mut rate) = (f32::MAX, 0.0f32, 0.0f32);
    let mut last: Option<(f32, Vec3)> = None;
    for &(t, p) in path {
        let los = p - eye;
        let d = los.length();
        min_d = min_d.min(d);
        max_d = max_d.max(d);
        let unit = los.normalized();
        if let Some((t0, u0)) = last {
            let angle = unit.dot(u0).clamp(-1.0, 1.0).acos();
            rate = rate.max(angle / (t - t0).max(1e-3));
        }
        last = Some((t, unit));
    }
    (min_d, max_d, rate.min(MAX_RATE * 4.0))
}
