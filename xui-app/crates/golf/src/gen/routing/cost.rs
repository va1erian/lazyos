//! The routing's constraints and costs, split so a move re-scores only what
//! it touched: [`single`] for one hole, [`pair`] for two, [`par_runs`] for
//! the par sequence. A cost's `violation` part must be zero for a valid
//! routing; `soft` is what the solver minimises among valid ones.

use std::ops::AddAssign;

use crate::field::Field;
use crate::math::{polyline_at, polyline_distance, Vec2};

use super::Route;

/// Property margin: corridors stay this far inside the site edge.
pub const MARGIN: f32 = 45.0;
/// Around the clubhouse, holes 1, 9, 10 and 18 may meet.
pub const CLUBHOUSE_ZONE: f32 = 70.0;
/// Hole n+1's tee is at most this far from hole n's green.
pub const MAX_WALK: f32 = 80.0;
/// Holes closer than this (centerline to centerline) cost a little: with
/// trees between them, holes this far apart read as separate.
pub const ROOMY: f32 = 95.0;

/// A line of play keeps this far from a landmark's core on both sides.
pub const PLAY_CLEARANCE: f32 = 18.0;

/// What a straight par 4 or 5 costs the routing.
pub const STRAIGHT: f32 = 4.0;

/// The least distance from the clubhouse to a tee or green centre.
pub const CLUBHOUSE_CLEAR: f32 = 45.0;

/// The least distance between any two tee or green centres.
pub const SPOT_GAP: f32 = 34.0;

/// The site the routing is solved on.
pub struct Site<'a> {
    pub elevation: &'a Field<f32>,
    pub water: &'a Field<bool>,
    pub size: f32,
    pub clubhouse: Vec2,
    /// Minimum distance between two holes' centerlines.
    pub buffer: f32,
    /// The steepest slope a tee or green may sit on.
    pub max_slope: f32,
    /// Landmark cores (hills, crags, old woods) no play may cross.
    pub blocked: &'a Field<bool>,
    /// The relief normal for the site's landform (`Archetype::relief_scale`).
    pub relief: f32,
    /// The front nine keeps to the half of the site this unit vector points
    /// into from the clubhouse, the back nine to the other half (a zero
    /// vector lets the nines mix).
    pub front: Vec2,
}

impl Site<'_> {
    /// How far the site extends from the clubhouse along `dir` before the
    /// property margin.
    pub fn room(&self, dir: Vec2) -> f32 {
        let (lo, hi) = (MARGIN, self.size - MARGIN);
        let along = |p: f32, d: f32| {
            if d > 1e-4 {
                (hi - p) / d
            } else if d < -1e-4 {
                (lo - p) / d
            } else {
                f32::MAX
            }
        };
        along(self.clubhouse.x, dir.x)
            .min(along(self.clubhouse.y, dir.y))
            .max(0.0)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cost {
    pub violation: f32,
    pub soft: f32,
}

impl Cost {
    pub fn total(self) -> f32 {
        self.violation * 1000.0 + self.soft
    }
}

impl AddAssign for Cost {
    fn add_assign(&mut self, o: Cost) {
        self.violation += o.violation;
        self.soft += o.soft;
    }
}

/// The length band for `par`, metres along the centerline.
pub fn band(par: u8) -> (f32, f32) {
    match par {
        3 => (110.0, 230.0),
        4 => (230.0, 440.0),
        _ => (430.0, 600.0),
    }
}

/// Distance outside `lo..=hi`, 0 inside.
fn outside(value: f32, lo: f32, hi: f32) -> f32 {
    (lo - value).max(0.0) + (value - hi).max(0.0)
}

/// One hole's own constraints and costs; `index` is 0-based.
pub fn single(site: &Site, index: usize, route: &Route) -> Cost {
    single_noting(site, index, route, &mut |_, _| {})
}

/// [`single`], telling `note` what each violated constraint contributed
/// (for diagnosing a routing that will not fit).
pub fn single_noting(
    site: &Site,
    index: usize,
    route: &Route,
    note: &mut dyn FnMut(&'static str, f32),
) -> Cost {
    let mut cost = Cost::default();
    let mut violate = |cost: &mut Cost, what: &'static str, amount: f32| {
        if amount > 0.0 {
            cost.violation += amount;
            note(what, amount);
        }
    };
    let line = route.line();
    let inset = MARGIN;
    for p in &line {
        let out = outside(p.x, inset, site.size - inset) + outside(p.y, inset, site.size - inset);
        violate(&mut cost, "bounds", out);
    }
    let (lo, hi) = band(route.par);
    violate(&mut cost, "length", outside(route.length(), lo, hi));
    for spot in [route.tee, route.green] {
        if site.water.at(spot.x, spot.y) {
            violate(&mut cost, "water", 50.0);
        }
        // Over 12 m: sculpting flattens the small bumps, not the hillside.
        let slope = site.elevation.slope(spot.x, spot.y, 12.0);
        violate(
            &mut cost,
            "slope",
            (slope - site.max_slope).max(0.0) * 400.0,
        );
        cost.soft += slope * 40.0; // earthwork
    }
    let to_clubhouse = |p: Vec2| p.distance(site.clubhouse);
    // Tees and greens keep clear of the building and its terrace.
    for spot in [route.tee, route.green] {
        violate(
            &mut cost,
            "clubhouse",
            (CLUBHOUSE_CLEAR - to_clubhouse(spot)).max(0.0),
        );
    }
    if index == 0 || index == 9 {
        violate(
            &mut cost,
            "first tee",
            (to_clubhouse(route.tee) - 120.0).max(0.0),
        );
        // The first shot plays away from the clubhouse, not over it.
        let out = (route.tee - site.clubhouse).normalized();
        let first = (line[1] - line[0]).normalized();
        cost.soft += (0.2 - out.dot(first)).max(0.0) * 20.0;
    }
    if index == 8 || index == 17 {
        violate(
            &mut cost,
            "home green",
            (to_clubhouse(route.green) - 160.0).max(0.0),
        );
    }
    if site.front != Vec2::default() {
        let side = if index < 9 { 1.0 } else { -1.0 };
        let length = crate::math::polyline_length(&line);
        let mut s = 0.0;
        while s <= length {
            let (p, _) = polyline_at(&line, s);
            s += 25.0;
            if p.distance(site.clubhouse) < CLUBHOUSE_ZONE {
                continue;
            }
            let over = -(p - site.clubhouse).dot(site.front) * side - 40.0;
            violate(&mut cost, "half", over.max(0.0) * 0.2);
        }
    }
    for w in line.windows(3) {
        let a = (w[1] - w[0]).normalized();
        let b = (w[2] - w[1]).normalized();
        // A corner sharper than about 50 degrees is not a golf hole.
        violate(&mut cost, "corner", (0.64 - a.dot(b)).max(0.0) * 100.0);
    }
    // Each leg of a bent hole is long enough to play.
    if line.len() > 2 {
        for w in line.windows(2) {
            violate(&mut cost, "leg", (60.0 - w[0].distance(w[1])).max(0.0));
        }
    }
    violate(&mut cost, "blocked", blocked(site, &line));
    // Most long holes should bend; a straight one costs a little.
    if route.par > 3 && route.dogleg.is_none() {
        cost.soft += STRAIGHT;
    }
    cost.soft -= interest(site, route);
    // Holes that play over the land well: rolling, with a view from the
    // tee; never blind over a hill or landing on a side slope.
    let view = crate::gen::quality::hole(site, route);
    cost.soft += 14.0 * view.blind + 10.0 * view.side_slope - 5.0 * view.relief - 5.0 * view.vista;
    cost
}

/// How much of a line of play, and the ground either side of it, crosses a
/// landmark's blocked core.
pub fn blocked(site: &Site, line: &[Vec2]) -> f32 {
    if site.blocked.width == 0 {
        return 0.0;
    }
    let length = crate::math::polyline_length(line);
    let mut hits = 0.0;
    let mut s = 0.0;
    while s <= length {
        let (p, dir) = polyline_at(line, s);
        s += 8.0;
        for side in [-PLAY_CLEARANCE, 0.0, PLAY_CLEARANCE] {
            let q = p + dir.perp() * side;
            if site.blocked.at(q.x, q.y) {
                hits += 1.0;
            }
        }
    }
    hits * 8.0
}

/// Whether a line of play keeps clear of every landmark core (stops at
/// the first hit: the detour asks this of many trial corners).
pub fn is_clear(site: &Site, line: &[Vec2]) -> bool {
    if site.blocked.width == 0 {
        return true;
    }
    let length = crate::math::polyline_length(line);
    let mut s = 0.0;
    while s <= length {
        let (p, dir) = polyline_at(line, s);
        s += 8.0;
        for side in [0.0, -PLAY_CLEARANCE, PLAY_CLEARANCE] {
            let q = p + dir.perp() * side;
            if site.blocked.at(q.x, q.y) {
                return false;
            }
        }
    }
    true
}

/// The reward for an interesting hole: water to carry or beside the green,
/// and an elevated tee.
fn interest(site: &Site, route: &Route) -> f32 {
    let line = route.line();
    let mut reward = 0.0;
    let carry = if route.par == 3 {
        (40.0, route.length() - 15.0)
    } else {
        (110.0, 210.0)
    };
    let mut s = carry.0;
    while s < carry.1 {
        let (p, _) = polyline_at(&line, s);
        if site.water.at(p.x, p.y) {
            reward += 10.0;
            break;
        }
        s += 6.0;
    }
    for k in 0..8 {
        let around = route.green + Vec2::from_angle(k as f32 * 0.785) * 28.0;
        if site.water.at(around.x, around.y) {
            reward += 4.0;
            break;
        }
    }
    let (landing, _) = polyline_at(&line, route.length().min(200.0));
    let drop = site.elevation.sample(route.tee.x, route.tee.y)
        - site.elevation.sample(landing.x, landing.y);
    if drop > 4.0 {
        reward += 5.0;
    }
    reward
}

/// Constraints and costs between holes `i` and `j` (`i < j`).
pub fn pair(site: &Site, i: usize, a: &Route, j: usize, b: &Route) -> Cost {
    let mut cost = Cost::default();
    let consecutive = j == i + 1 && j != 9;
    let (la, lb) = (a.line(), b.line());
    let near_clubhouse = |p: Vec2| p.distance(site.clubhouse) < CLUBHOUSE_ZONE;
    // Where the two holes hand over (green i to tee j), they may be close.
    let exempt = |p: Vec2| {
        near_clubhouse(p)
            || (consecutive && (p.distance(a.green) < 45.0 || p.distance(b.tee) < 45.0))
    };
    for (line, other) in [(&lb, &la), (&la, &lb)] {
        let (violation, crowding) = overlap(line, other, site.buffer, &exempt);
        cost.violation += violation;
        cost.soft += crowding;
    }
    // Tees and greens never touch, even by the clubhouse where corridors may.
    for (p, q) in [
        (a.tee, b.tee),
        (a.green, b.green),
        (a.tee, b.green),
        (a.green, b.tee),
    ] {
        cost.violation += (SPOT_GAP - p.distance(q)).max(0.0);
    }
    if consecutive {
        let walk = a.green.distance(b.tee);
        cost.violation += outside(walk, SPOT_GAP, MAX_WALK);
        cost.soft += walk * 0.05;
        // Wind and sun variety: consecutive holes should not face alike.
        let turn = a.axis().dot(b.axis());
        if turn > 0.87 {
            cost.soft += 6.0;
        }
    }
    cost
}

/// How far points sampled along `line` intrude into `other`'s buffer (a
/// violation), and how much they crowd it short of [`ROOMY`] (a cost).
fn overlap(
    line: &[Vec2],
    other: &[Vec2],
    buffer: f32,
    exempt: &dyn Fn(Vec2) -> bool,
) -> (f32, f32) {
    let length = crate::math::polyline_length(line);
    let mut s = 0.0;
    let (mut sum, mut crowding) = (0.0, 0.0);
    while s <= length {
        let (p, _) = polyline_at(line, s);
        s += 10.0;
        if exempt(p) {
            continue;
        }
        let (d, along) = polyline_distance(p, other);
        if d < ROOMY {
            let (q, _) = polyline_at(other, along);
            if !exempt(q) {
                sum += (buffer - d).max(0.0);
                crowding += ROOMY - d;
            }
        }
    }
    (sum * 0.2, crowding * 0.015)
}

/// Runs of the same par: three in a row costs, four costs more.
pub fn par_runs(pars: &[u8]) -> f32 {
    let mut cost = 0.0;
    for w in pars.windows(3) {
        if w[0] == w[1] && w[1] == w[2] {
            cost += 5.0;
        }
    }
    if pars.first() == Some(&3) {
        cost += 3.0;
    }
    cost
}

/// The whole routing's cost.
pub fn total(site: &Site, routes: &[Route]) -> Cost {
    let mut cost = Cost::default();
    for (i, route) in routes.iter().enumerate() {
        cost += single(site, i, route);
        for (j, other) in routes.iter().enumerate().skip(i + 1) {
            cost += pair(site, i, route, j, other);
        }
    }
    let pars: Vec<u8> = routes.iter().map(|r| r.par).collect();
    cost.soft += par_runs(&pars);
    cost
}
