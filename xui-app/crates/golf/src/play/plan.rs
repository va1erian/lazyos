//! The caddy's plan for a stroke: where to aim, with which club. It tries
//! targets from the furthest the ball can be sent back toward the player,
//! for each picks the shortest club that has the power in hand, plays the
//! shot out with the simulator and keeps the first whose ball is never
//! anywhere it should not be. The human sees this as the club and aim the
//! game starts them with; the computer player takes it as is.

use crate::course::{Hole, Material};
use crate::math::{polyline_at, polyline_distance, polyline_length, Vec2, Vec3};

use super::ball::{Event, Flight, Wind};
use super::caddy;
use super::clubs::{BAG, CLUBS};
use super::shot::play_out;
use super::solve;
use super::surface;
use super::terrain::{Filtered, Terrain};

/// A landing or a rest on a slope steeper than this runs off it (rise
/// over run), off the green.
const MAX_SLOPE: f32 = 0.14;
/// A club is used only when the shot needs at most this share of its
/// swing, so the player has some in hand.
const IN_HAND: f32 = 0.97;
/// Longer clubs than the recommended one that are tried when it falls
/// short.
const LONGER: usize = 3;
/// A shot at least this good is taken at once: a steep landing, which
/// is only a little bad, is allowed; a bunker, the water or the out of
/// bounds (20 or more each) is not.
const GOOD_ENOUGH: i32 = -15;
/// Furthest and nearest the lay-up targets go, metres, and their spacing.
const NEAREST: f32 = 40.0;
const STEP: f32 = 10.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    pub heading: Vec2,
    /// Where the ball is meant to finish, metres.
    pub distance: f32,
    /// Index into [`BAG`].
    pub club: usize,
    /// Whether the plan is known to end well (otherwise it is the best
    /// the caddy could find, or only the first candidate).
    pub safe: bool,
}

fn hazard(material: Material) -> bool {
    matches!(
        material,
        Material::Water
            | Material::OutOfBounds
            | Material::Sand
            | Material::Woodland
            | Material::DeepRough
            | Material::Rock
            | Material::WasteArea
    )
}

fn slope(terrain: &dyn Terrain, p: Vec3) -> f32 {
    let n = terrain.normal(p.x, p.z);
    n.x.hypot(n.z) / n.y
}

/// How well a shot turns out, from 0 (nothing wrong) down: in the water or
/// out of bounds is far worse than any bunker or steep landing, which are
/// worse than none. A holed shot is best of all.
fn score(flight: &Flight, terrain: &dyn Terrain) -> i32 {
    match flight.end() {
        Event::Holed => return 100,
        Event::Splash | Event::OutOfBounds => return -1000,
        _ => {}
    }
    let bad = |p: Vec3| hazard(terrain.material(p.x, p.z));
    let mut s = 0;
    for sample in &flight.samples {
        if matches!(sample.event, Event::Land | Event::Bounce | Event::Rest) && bad(sample.position)
        {
            s -= 20;
        }
    }
    // Somewhere along the roll, too: a ball that runs through a bunker
    // and out the other side is in the bunker for a moment.
    s -= 5 * flight
        .samples
        .iter()
        .step_by(30)
        .filter(|sample| bad(sample.position))
        .count() as i32;
    let end = flight
        .samples
        .last()
        .map_or(Vec3::default(), |s| s.position);
    let on_green = terrain.material(end.x, end.z) == Material::Green;
    if !on_green {
        let steep = (slope(terrain, end) - MAX_SLOPE).max(0.0);
        s -= (steep * 100.0).min(40.0) as i32;
    }
    s
}

/// The targets to try, best first: the pin if it is in reach, then points
/// of the line of play from as far as the longest club reaches back to
/// nearer ones.
fn targets(hole: &Hole, at: Vec2) -> Vec<(Vec2, f32)> {
    let reach = CLUBS[0].carry_m * 1.1;
    let mut out = Vec::new();
    let to_pin = at.distance(hole.pin);
    if to_pin <= reach {
        out.push(((hole.pin - at).normalized(), to_pin));
    }
    let (_, along) = polyline_distance(at, &hole.centerline);
    let length = polyline_length(&hole.centerline);
    let mut ahead = CLUBS[0].carry_m * 0.95;
    while ahead >= NEAREST {
        let (target, _) = polyline_at(&hole.centerline, (along + ahead).min(length));
        let distance = at.distance(target);
        if distance <= reach && distance > 15.0 {
            out.push(((target - at).normalized(), distance));
        }
        ahead -= STEP;
    }
    if out.is_empty() {
        out.push(caddy::default_aim(hole, at));
    }
    out
}

/// The club for a shot of `distance` from `at`: the recommended one or
/// the first longer one that has the power in hand. And that power.
fn pick_club(
    terrain: &dyn Terrain,
    at: Vec3,
    heading: Vec2,
    distance: f32,
    material: Material,
    wind: Wind,
    tee: bool,
) -> (usize, f32) {
    let mut first = caddy::recommend(distance, tee);
    if material == Material::Sand {
        first = first.max(9);
    }
    let mut best = (first, 1.0);
    for step in 0..=LONGER {
        let Some(club) = first.checked_sub(step) else {
            break;
        };
        let lie = surface::lie(material, &BAG[club]);
        let power = solve::power_for(&BAG[club], terrain, at, heading, distance, wind, &lie);
        best = (club, power);
        if power <= IN_HAND {
            break;
        }
    }
    best
}

/// The plan for a ball at `at`, on `material`, on a hole whose pin and
/// line of play are `hole`'s.
pub fn plan(
    hole: &Hole,
    terrain: &dyn Terrain,
    at: Vec3,
    material: Material,
    wind: Wind,
    tee: bool,
) -> Plan {
    let play_terrain = Filtered {
        inner: terrain,
        trees: true,
    };
    let mut best: Option<(i32, Plan)> = None;
    for (heading, distance) in targets(hole, at.ground()) {
        let (club, power) = pick_club(terrain, at, heading, distance, material, wind, tee);
        let lie = surface::lie(material, &BAG[club]);
        let launch = solve::launch(&BAG[club], at, heading, power, 0.0, &lie);
        let flight = play_out(&launch, wind, &play_terrain);
        let score = score(&flight, terrain);
        let plan = Plan {
            heading,
            distance,
            club,
            safe: score >= GOOD_ENOUGH,
        };
        if score >= GOOD_ENOUGH {
            return plan;
        }
        // The furthest of equally bad candidates comes first, and stays.
        if best.is_none_or(|(b, _)| score > b) {
            best = Some((score, plan));
        }
    }
    best.expect("targets never returns none").1
}
