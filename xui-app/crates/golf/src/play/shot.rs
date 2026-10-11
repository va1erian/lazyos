//! A whole shot: the carry, the bounces and the roll, ending at rest, in
//! the water, out of bounds or in the cup. Simulated once at impact into a
//! [`Flight`], so the camera can see the end before the ball gets there.

use std::cell::Cell;

use crate::math::{Vec2, Vec3};

use super::ball::{leg, Contact, Event, Flight, Launch, Sample, State, Wind, GRAVITY, RADIUS, RPM};
use super::surface;
use super::terrain::Terrain;
use crate::course::Material;

/// The cup's radius, metres (4.25 inches across).
pub const HOLE_RADIUS: f32 = 0.054;
/// A ball faster than this over the cup does not drop.
pub const CAPTURE_SPEED: f32 = 1.6;
const ROLL_STEP: f32 = 1.0 / 120.0;
/// A rolling ball is given up on after this long, seconds.
const MAX_ROLL: f32 = 90.0;
/// A bounce slower than this (into the surface) is the start of a roll.
const MIN_HOP: f32 = 1.2;
const MAX_HOPS: u32 = 12;
/// Trees within this of the launch are the tee's own, not hit.
const CLEAR_START: f32 = 3.0;

/// The ball from `launch` to wherever it stops.
pub fn play_out(launch: &Launch, wind: Wind, terrain: &dyn Terrain) -> Flight {
    let mut state = State::launched(launch);
    let mut out = vec![Sample {
        t: 0.0,
        position: state.position,
        velocity: state.velocity,
        event: Event::None,
    }];
    let ground = |x: f32, z: f32| terrain.height(x, z);
    let start = launch.position.ground();
    let last_hit = Cell::new(f32::MIN);
    let solid = |p: Vec3, t: f32| {
        t > last_hit.get() + 0.5 && p.ground().distance(start) > CLEAR_START && terrain.solid(p)
    };

    let mut landed = false;
    let mut hops = 0;
    // A putt starts on the ground: no leg.
    let mut airborne = launch.angle > 0.01;
    while airborne {
        match leg(&mut state, wind, &ground, &solid, &mut out) {
            Contact::Timeout => return finish(out, Event::Rest),
            Contact::Solid => {
                last_hit.set(state.t);
                // The ball loses most of its speed and drops, turned back.
                let v = state.velocity;
                state.velocity = Vec3::new(-v.x * 0.25, v.y.min(0.0) * 0.2, -v.z * 0.25);
                label(&mut out, Event::TreeHit);
            }
            Contact::Ground => {
                let p = state.position;
                label(&mut out, if landed { Event::Bounce } else { Event::Land });
                landed = true;
                match terrain.material(p.x, p.z) {
                    Material::Water => return finish(out, Event::Splash),
                    Material::OutOfBounds => return finish(out, Event::OutOfBounds),
                    m => {
                        hops += 1;
                        airborne = bounce(&mut state, terrain, m) && hops < MAX_HOPS;
                    }
                }
            }
        }
    }
    roll(&mut state, terrain, &mut out);
    Flight { samples: out }
}

fn label(out: &mut [Sample], event: Event) {
    if let Some(last) = out.last_mut() {
        last.event = event;
    }
}

fn finish(mut out: Vec<Sample>, event: Event) -> Flight {
    label(&mut out, event);
    Flight { samples: out }
}

/// The ball hits `material`: its velocity leaves as a bounce. Returns
/// whether it hops (otherwise it rolls on).
fn bounce(state: &mut State, terrain: &dyn Terrain, material: Material) -> bool {
    let surface = surface::of(material);
    let p = state.position;
    let n = terrain.normal(p.x, p.z);
    let v = state.velocity;
    let into = v.dot(n);
    if into >= 0.0 {
        return false;
    }
    let tangent = v - n * into;
    let speed = tangent.length();
    let along = if speed > 1e-3 {
        tangent * (1.0 / speed)
    } else {
        Vec3::default()
    };
    // Backspin bites on the way through: it takes speed off the forward
    // motion, and can pull the ball back, but never more than half again.
    let bite = surface.grip * (state.backspin / RPM) / 1000.0 * 1.8;
    let kept = (speed * surface.impact_keep - bite).max(-0.5 * speed);
    let rebound = -into * surface.restitution;
    state.velocity = along * kept + n * rebound;
    state.backspin *= 0.5;
    state.sidespin *= 0.3;
    rebound > MIN_HOP
}

/// The distance from `p` to the segment `a`-`b`.
fn segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    crate::math::segment_distance(p, a, b).0
}

/// The ball on the ground: slope accelerates it, the surface slows it, a
/// slow ball over the cup drops, and it ends at rest, in water or out.
fn roll(state: &mut State, terrain: &dyn Terrain, out: &mut Vec<Sample>) {
    let pin = terrain.pin();
    let g = Vec3::new(0.0, -GRAVITY, 0.0);
    let end = state.t + MAX_ROLL;
    while state.t < end {
        let p = state.position;
        let n = terrain.normal(p.x, p.z);
        // Along the surface only.
        let mut v = state.velocity - n * state.velocity.dot(n);
        let slope = g - n * g.dot(n);
        let mu = surface::of(terrain.material(p.x, p.z)).roll;
        let decel = mu * GRAVITY * n.y;
        // Gravity along the slope first, then friction against the net
        // motion: where friction holds the ball it stops, rather than
        // being nudged again by the slope after it was stopped.
        v = v + slope * ROLL_STEP;
        let speed = v.length();
        if speed > 1e-3 {
            let drop = (decel * ROLL_STEP).min(speed);
            v = v - v * (drop / speed);
        }
        if v.length() < 0.03 && slope.length() <= decel {
            state.velocity = Vec3::default();
            out.push(sample(state, Event::Rest));
            return;
        }
        let next = p + v * ROLL_STEP;
        let (x, z) = (next.x, next.z);
        let at = Vec3::new(x, terrain.height(x, z) + RADIUS, z);
        state.t += ROLL_STEP;
        state.position = at;
        state.velocity = v;
        if let Some(pin) = pin {
            let near = segment_distance(pin, p.ground(), at.ground());
            if near <= HOLE_RADIUS && v.length() <= CAPTURE_SPEED {
                state.position = Vec3::new(pin.x, terrain.height(pin.x, pin.y) - 0.04, pin.y);
                out.push(sample(state, Event::Holed));
                return;
            }
        }
        match terrain.material(x, z) {
            Material::Water => {
                out.push(sample(state, Event::Splash));
                return;
            }
            Material::OutOfBounds => {
                out.push(sample(state, Event::OutOfBounds));
                return;
            }
            _ => out.push(sample(state, Event::None)),
        }
    }
    label(out, Event::Rest);
}

fn sample(state: &State, event: Event) -> Sample {
    Sample {
        t: state.t,
        position: state.position,
        velocity: state.velocity,
        event,
    }
}
