//! Stage 3, routing: where the 18 holes go. A beam search lays the holes
//! one by one from the clubhouse; simulated annealing then refines the
//! routing it found (docs/golf-course-generator.md, "Routing").

mod anneal;
pub mod cost;
mod detour;

use crate::math::Vec2;
use crate::rng::Rng;

pub use cost::Site;

/// One hole's corridor: a tee, an optional dogleg corner (and, for an
/// S-shaped hole, a second bend after it) and a green.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Route {
    pub tee: Vec2,
    pub dogleg: Option<Vec2>,
    /// A second corner after `dogleg` (only with one).
    pub bend: Option<Vec2>,
    pub green: Vec2,
    /// The par the routing aims for; the par solver has the last word.
    pub par: u8,
}

impl Route {
    /// The centerline's control points, tee to green.
    pub fn line(&self) -> Vec<Vec2> {
        let mut line = vec![self.tee];
        line.extend(self.dogleg);
        line.extend(self.dogleg.and(self.bend));
        line.push(self.green);
        line
    }

    pub fn length(&self) -> f32 {
        crate::math::polyline_length(&self.line())
    }

    /// Tee to green.
    pub fn axis(&self) -> Vec2 {
        (self.green - self.tee).normalized()
    }
}

/// The par sequence: about 4 par-3s, 10 par-4s and 4 par-5s, varied by one.
pub fn par_mix(rng: &mut Rng) -> Vec<u8> {
    let (threes, fives) = match rng.below(5) {
        0 => (5, 4),
        1 => (4, 5),
        2 => (5, 5),
        _ => (4, 4),
    };
    let mut best: Vec<u8> = Vec::new();
    let mut best_cost = f32::MAX;
    // Two nines of their own mix, shuffled until no par repeats thrice.
    for _ in 0..64 {
        let mut pars = Vec::with_capacity(18);
        for (t, f) in [
            (threes / 2, fives / 2),
            (threes - threes / 2, fives - fives / 2),
        ] {
            let mut half = vec![3u8; t];
            half.extend(std::iter::repeat_n(5u8, f));
            half.extend(std::iter::repeat_n(4u8, 9 - t - f));
            for i in (1..half.len()).rev() {
                half.swap(i, rng.below(i + 1));
            }
            pars.extend(half);
        }
        let c = cost::par_runs(&pars);
        if c < best_cost {
            best_cost = c;
            best = pars;
        }
        if c == 0.0 {
            break;
        }
    }
    best
}

/// Whether a landmark blocks play at `p`.
fn blocked_at(site: &Site, p: Vec2) -> bool {
    site.blocked.width > 0 && site.blocked.at(p.x, p.y)
}

/// The flattest dry spot near the middle of the site, for the clubhouse.
pub fn clubhouse(site: &Site, rng: &mut Rng) -> Vec2 {
    let mut best = (f32::MAX, Vec2::new(site.size / 2.0, site.size / 2.0));
    for _ in 0..600 {
        let p = Vec2::new(
            rng.range(site.size * 0.3, site.size * 0.7),
            rng.range(site.size * 0.3, site.size * 0.7),
        );
        let mut score = 0.0;
        for k in 0..9 {
            let q = p + Vec2::from_angle(k as f32 * 0.7) * (k as f32 * 6.0);
            score += site.elevation.slope(q.x, q.y, 6.0);
            if site.water.at(q.x, q.y) {
                score += 5.0;
            }
        }
        // Never on a crag top or in an old wood: four holes start and end here.
        for k in 0..12 {
            let q = p + Vec2::from_angle(k as f32 * std::f32::consts::FRAC_PI_6) * 70.0;
            if blocked_at(site, p) || blocked_at(site, q) {
                score += 10.0;
            }
        }
        score += p.distance(Vec2::new(site.size / 2.0, site.size / 2.0)) * 0.0004;
        if score < best.0 {
            best = (score, p);
        }
    }
    best.1
}

/// One partial routing in the beam.
#[derive(Clone)]
struct Partial {
    routes: Vec<Route>,
    score: f32,
}

const BEAM: usize = 24;
const CANDIDATES: usize = 48;

/// Routes 18 holes over the site, or `None` when the beam ran dry.
pub fn beam_search(site: &Site, pars: &[u8], rng: &mut Rng) -> Option<Vec<Route>> {
    let mut beam = vec![Partial {
        routes: Vec::new(),
        score: 0.0,
    }];
    for (k, &par) in pars.iter().enumerate() {
        let mut next: Vec<Partial> = Vec::new();
        for state in &beam {
            for _ in 0..CANDIDATES {
                let route = candidate(site, state.routes.last(), k, par, rng);
                let Some(score) = extend_score(site, &state.routes, k, &route, pars.len()) else {
                    continue;
                };
                let mut routes = state.routes.clone();
                routes.push(route);
                next.push(Partial {
                    routes,
                    score: state.score + score,
                });
            }
        }
        if next.is_empty() {
            return None;
        }
        next.sort_by(|a, b| a.score.total_cmp(&b.score));
        beam = diverse(next);
    }
    beam.into_iter().next().map(|p| p.routes)
}

/// The best [`BEAM`] states, skipping any whose last green is within 20 m
/// of a better state's: a beam of near-twins dies all at once.
fn diverse(sorted: Vec<Partial>) -> Vec<Partial> {
    let mut kept: Vec<Partial> = Vec::with_capacity(BEAM);
    for state in sorted {
        let green = state.routes.last().map(|r| r.green);
        let twin = kept.iter().any(|k| match (k.routes.last(), green) {
            (Some(a), Some(b)) => a.green.distance(b) < 20.0,
            _ => false,
        });
        if !twin {
            kept.push(state);
            if kept.len() == BEAM {
                break;
            }
        }
    }
    kept
}

/// The score of adding `route` as hole `k`, or `None` if it breaks a hard
/// constraint (or can no longer get back to the clubhouse in time).
fn extend_score(
    site: &Site,
    placed: &[Route],
    k: usize,
    route: &Route,
    holes: usize,
) -> Option<f32> {
    let mut c = cost::single(site, k, route);
    for (j, other) in placed.iter().enumerate() {
        c += cost::pair(site, j, other, k, route);
    }
    if c.violation > 0.0 {
        return None;
    }
    // Each nine must come home: the green may not wander further from the
    // clubhouse than the holes left in the nine can bring it back.
    let nine_end = if k < 9 { 8 } else { holes - 1 };
    let left = (nine_end - k) as f32;
    if route.green.distance(site.clubhouse) > left * 280.0 + 150.0 {
        return None;
    }
    // Each nine follows its loop of waypoints out and back; a mild pull
    // keeps the beam from wandering where the return will not fit.
    let pull = route.green.distance(waypoint(site, k)) * 0.05;
    Some(c.soft + pull)
}

/// Where hole `k`'s green would sit on its nine's loop: out from the
/// clubhouse into the nine's half on one side, back on the other.
fn waypoint(site: &Site, k: usize) -> Vec2 {
    let side = if k < 9 { 1.0 } else { -1.0 };
    let out = if site.front == Vec2::default() {
        Vec2::new(1.0, 0.0)
    } else {
        site.front * side
    };
    let across = out.perp();
    let t = ((k % 9) + 1) as f32 / 9.0;
    let depth = (site.room(out) - 40.0).max(120.0);
    let swing = site.room(across).min(site.room(across * -1.0)).max(100.0) * 0.6;
    let pi = std::f32::consts::PI;
    site.clubhouse + out * (depth * (pi * t).sin()) + across * (swing * (2.0 * pi * t).sin())
}

/// The split between the nines: of a few directions, the one leaving the
/// most room on both sides of the clubhouse.
pub fn split(site: &Site, rng: &mut Rng) -> Vec2 {
    let offset = rng.range(0.0, std::f32::consts::PI);
    (0..8)
        .map(|k| Vec2::from_angle(offset + k as f32 * std::f32::consts::PI / 8.0))
        .max_by(|a, b| {
            let room = |d: Vec2| site.room(d).min(site.room(d * -1.0));
            room(*a).total_cmp(&room(*b))
        })
        .unwrap_or(Vec2::new(1.0, 0.0))
}

/// A random hole `k` of `par` starting near the previous green (or the
/// clubhouse for holes 1 and 10). A few draws are tried until one, bent
/// around any landmark in its way, has its tee, corners and green on dry,
/// open land inside the property.
fn candidate(site: &Site, previous: Option<&Route>, k: usize, par: u8, rng: &mut Rng) -> Route {
    let inside = |p: Vec2| {
        let (lo, hi) = (cost::MARGIN, site.size - cost::MARGIN);
        (lo..hi).contains(&p.x)
            && (lo..hi).contains(&p.y)
            && !site.water.at(p.x, p.y)
            && !blocked_at(site, p)
    };
    let mut route = draw(site, previous, k, par, rng);
    for _ in 0..8 {
        // A hole that runs into a landmark bends around it.
        if let Some(bent) = detour::around(site, route) {
            if bent.line().into_iter().all(inside) {
                return bent;
            }
        }
        route = draw(site, previous, k, par, rng);
    }
    route
}

fn draw(site: &Site, previous: Option<&Route>, k: usize, par: u8, rng: &mut Rng) -> Route {
    let start = match previous {
        Some(prev) if k != 9 => prev.green,
        _ => site.clubhouse,
    };
    // From the clubhouse the tee goes out toward the nine's loop, a little
    // further than a walk from a green, so it clears the terrace.
    let tee = if start == site.clubhouse {
        let toward = (waypoint(site, k) - site.clubhouse).angle() + rng.gaussian() * 0.7;
        start + Vec2::from_angle(toward) * rng.range(48.0, 95.0)
    } else {
        start + Vec2::from_angle(rng.range(0.0, std::f32::consts::TAU)) * rng.range(30.0, 70.0)
    };
    if k == 8 || k == 17 {
        return closing(site, tee, par, rng);
    }
    // Mostly toward the hole's waypoint on its nine's loop, sometimes anywhere.
    let angle = if rng.chance(0.7) {
        (waypoint(site, k) - tee).angle() + rng.gaussian() * 0.6
    } else {
        rng.range(0.0, std::f32::consts::TAU)
    };
    let heading = Vec2::from_angle(angle);
    let (lo, hi) = match par {
        3 => (125.0, 205.0),
        4 => (280.0, 410.0),
        _ => (450.0, 545.0),
    };
    let length = rng.range(lo, hi);
    let side = |rng: &mut Rng| if rng.chance(0.5) { 1.0 } else { -1.0 };
    // Most holes bend; long ones may bend twice, either way.
    let (one, two) = match par {
        3 => (0.25, 0.0),
        4 => (0.75, if length > 360.0 { 0.25 } else { 0.0 }),
        _ => (0.9, 0.55),
    };
    if !rng.chance(one) {
        return Route {
            tee,
            dogleg: None,
            bend: None,
            green: tee + heading * length,
            par,
        };
    }
    if par == 3 {
        // A par 3 only drifts: a gentle bend along the shot.
        let corner = tee + heading * (length * 0.5);
        let green = corner
            + Vec2::from_angle(heading.angle() + rng.range(0.1, 0.3) * side(rng)) * (length * 0.5);
        return Route {
            tee,
            dogleg: Some(corner),
            bend: None,
            green,
            par,
        };
    }
    let first_turn = rng.range(0.3, 0.8) * side(rng);
    if rng.chance(two) {
        // An S: out, one way, then back the other (or a sweeping C).
        let a = (length * rng.range(0.38, 0.48)).min(250.0);
        let b = length * rng.range(0.25, 0.35);
        let corner = tee + heading * a;
        let mid = Vec2::from_angle(heading.angle() + first_turn);
        let bend = corner + mid * b;
        let back = if rng.chance(0.65) { -1.0 } else { 1.0 };
        let last =
            Vec2::from_angle(mid.angle() + first_turn.signum() * back * rng.range(0.3, 0.75));
        return Route {
            tee,
            dogleg: Some(corner),
            bend: Some(bend),
            green: bend + last * (length - a - b),
            par,
        };
    }
    let leg = (length * rng.range(0.45, 0.65)).min(260.0);
    let corner = tee + heading * leg;
    let second = Vec2::from_angle(heading.angle() + first_turn);
    Route {
        tee,
        dogleg: Some(corner),
        bend: None,
        green: corner + second * (length - leg),
        par,
    }
}

/// A hole that ends by the clubhouse: the green is picked first, near
/// home, and the corridor bends toward it if the straight line is short.
/// The target par is kept if the geometry allows, else whichever par the
/// length fits (the par solver has the last word anyway).
fn closing(site: &Site, tee: Vec2, par: u8, rng: &mut Rng) -> Route {
    let mut fallback = Route {
        tee,
        dogleg: None,
        bend: None,
        green: site.clubhouse,
        par,
    };
    for attempt in 0..32 {
        let green = site.clubhouse
            + Vec2::from_angle(rng.range(0.0, std::f32::consts::TAU)) * rng.range(50.0, 150.0);
        let direct = tee.distance(green);
        let bend = direct < cost::band(par).0 + 10.0 && par != 3;
        let route = if bend {
            // A corner off the straight line, never sharper than the
            // dogleg limit (checked below with the length).
            let off = rng.range(0.1, 0.3) * direct * if rng.chance(0.5) { 1.0 } else { -1.0 };
            let corner =
                tee.lerp(green, rng.range(0.45, 0.6)) + (green - tee).normalized().perp() * off;
            Route {
                tee,
                dogleg: Some(corner),
                bend: None,
                green,
                par,
            }
        } else {
            Route {
                tee,
                dogleg: None,
                bend: None,
                green,
                par,
            }
        };
        let length = route.length();
        let sharp = route
            .dogleg
            .is_some_and(|c| (c - tee).normalized().dot((green - c).normalized()) < 0.66);
        if sharp {
            continue;
        }
        let fits = |p: u8| {
            let (lo, hi) = cost::band(p);
            (lo..=hi).contains(&length)
        };
        if fits(par) {
            return route;
        }
        // Half the draws keep trying for the target par first.
        if attempt >= 16 {
            if let Some(other) = [4u8, 3, 5].into_iter().find(|&p| fits(p)) {
                return Route {
                    par: other,
                    ..route
                };
            }
        }
        fallback = route;
    }
    fallback
}

/// Routes the course: beam search with ever looser constraints until one
/// fits, then annealing. Always returns 18 holes, with how many times the
/// constraints were loosened (`None`: nothing fitted, the fallback grid).
pub fn route(site: &mut Site, pars: &[u8], rng: &mut Rng) -> (Vec<Route>, Option<usize>) {
    // The last rung barely constrains slope or spacing: a cramped course
    // beats the fallback grid. Each try re-draws the split between nines.
    // The first rungs keep holes far enough apart for trees between them;
    // steep sites get their slope allowance before losing any spacing.
    let loosen = [
        (74.0, 0.22),
        (66.0, 0.27),
        (58.0, 0.34),
        (50.0, 0.44),
        (42.0, 0.56),
        (30.0, 1.0),
    ];
    let mut found = None;
    'ladder: for (level, &(buffer, slope)) in loosen.iter().enumerate() {
        site.buffer = buffer;
        site.max_slope = slope;
        for _ in 0..2 {
            if let Some(routes) = beam_search(site, pars, rng) {
                found = Some((routes, level));
                break 'ladder;
            }
            site.front = split(site, rng);
        }
    }
    let level = found.as_ref().map(|f| f.1);
    let mut routes = found.map_or_else(|| fallback(site, pars), |f| f.0);
    anneal::refine(site, &mut routes, rng, 2500);
    (routes, level)
}

/// A last resort that always fits: holes zig-zag in rows across the site.
fn fallback(site: &Site, pars: &[u8]) -> Vec<Route> {
    let rows = 6;
    let pitch = (site.size - 2.0 * cost::MARGIN) / rows as f32;
    pars.iter()
        .enumerate()
        .map(|(i, &par)| {
            let row = i % rows;
            let column = i / rows;
            let y = cost::MARGIN + pitch * (row as f32 + 0.5);
            let x0 = cost::MARGIN + 20.0 + column as f32 * (site.size - 2.0 * cost::MARGIN) / 3.0;
            let length = match par {
                3 => 150.0,
                4 => 260.0,
                _ => 300.0,
            };
            Route {
                tee: Vec2::new(x0, y),
                dogleg: None,
                bend: None,
                green: Vec2::new(x0 + length, y),
                par,
            }
        })
        .collect()
}
