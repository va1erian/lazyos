//! Simulated annealing over a valid routing. Moves: nudge a green, nudge a
//! tee, move a dogleg corner, swap the pars of two holes. A move is scored
//! by re-evaluating only the hole it touched against the others.

use crate::rng::Rng;

use super::cost::{self, Cost, Site};
use super::Route;

/// The part of the cost that involves hole `i`.
fn touching(site: &Site, routes: &[Route], i: usize) -> Cost {
    let mut c = cost::single(site, i, &routes[i]);
    for (j, other) in routes.iter().enumerate() {
        if j < i {
            c += cost::pair(site, j, other, i, &routes[i]);
        } else if j > i {
            c += cost::pair(site, i, &routes[i], j, other);
        }
    }
    c
}

fn par_cost(routes: &[Route]) -> f32 {
    let pars: Vec<u8> = routes.iter().map(|r| r.par).collect();
    cost::par_runs(&pars)
}

/// Refines `routes` in place over `iterations` moves, keeping the best
/// routing seen (Metropolis may wander uphill at the end).
pub fn refine(site: &Site, routes: &mut [Route], rng: &mut Rng, iterations: usize) {
    let n = routes.len();
    if n < 2 {
        return;
    }
    let start_temperature = 6.0;
    let mut current = cost::total(site, routes).total();
    let mut best = (current, routes.to_vec());
    for step in 0..iterations {
        let temperature = start_temperature * (1.0 - step as f32 / iterations as f32) + 0.05;
        current += if rng.chance(0.08) {
            swap_pars(site, routes, rng, temperature)
        } else {
            nudge(site, routes, rng, temperature, start_temperature)
        };
        if current < best.0 {
            best = (current, routes.to_vec());
        }
    }
    routes.copy_from_slice(&best.1);
}

/// Moves one hole's green, tee or corner; returns the accepted cost change.
fn nudge(site: &Site, routes: &mut [Route], rng: &mut Rng, temperature: f32, start: f32) -> f32 {
    let i = rng.below(routes.len());
    let before = touching(site, routes, i).total();
    let saved = routes[i];
    let reach = 4.0 + 14.0 * temperature / start;
    let jitter =
        |rng: &mut Rng| crate::math::Vec2::new(rng.range(-reach, reach), rng.range(-reach, reach));
    match rng.below(3) {
        0 => routes[i].green = routes[i].green + jitter(rng),
        1 => routes[i].tee = routes[i].tee + jitter(rng),
        _ => match (routes[i].dogleg, routes[i].bend) {
            (Some(_), Some(bend)) if rng.chance(0.5) => routes[i].bend = Some(bend + jitter(rng)),
            (Some(corner), _) => routes[i].dogleg = Some(corner + jitter(rng)),
            (None, _) => routes[i].green = routes[i].green + jitter(rng),
        },
    }
    let delta = touching(site, routes, i).total() - before;
    if accept(delta, temperature, rng) {
        delta
    } else {
        routes[i] = saved;
        0.0
    }
}

/// Swaps two holes' target pars (the lengths must then fit the new bands);
/// returns the accepted cost change.
fn swap_pars(site: &Site, routes: &mut [Route], rng: &mut Rng, temperature: f32) -> f32 {
    let n = routes.len();
    let (a, b) = (rng.below(n), rng.below(n));
    if routes[a].par == routes[b].par {
        return 0.0;
    }
    let score = |routes: &[Route]| {
        touching(site, routes, a).total() + touching(site, routes, b).total()
            - pair_ab(site, routes, a, b)
            + par_cost(routes)
    };
    let before = score(routes);
    let (pa, pb) = (routes[a].par, routes[b].par);
    routes[a].par = pb;
    routes[b].par = pa;
    let delta = score(routes) - before;
    if accept(delta, temperature, rng) {
        delta
    } else {
        routes[a].par = pa;
        routes[b].par = pb;
        0.0
    }
}

/// The pair term between `a` and `b`, which both `touching` sums count.
fn pair_ab(site: &Site, routes: &[Route], a: usize, b: usize) -> f32 {
    let (i, j) = (a.min(b), a.max(b));
    cost::pair(site, i, &routes[i], j, &routes[j]).total()
}

fn accept(delta: f32, temperature: f32, rng: &mut Rng) -> bool {
    delta <= 0.0 || rng.next_f32() < (-delta / temperature).exp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::Field;
    use crate::math::Vec2;

    #[test]
    fn annealing_never_makes_a_routing_worse_overall() {
        let elevation = Field::new(600, 600, 0.0f32);
        let water = Field::new(600, 600, false);
        let blocked = Field::new(0, 0, false);
        let site = Site {
            elevation: &elevation,
            water: &water,
            size: 600.0,
            clubhouse: Vec2::new(300.0, 300.0),
            buffer: 60.0,
            max_slope: 0.1,
            relief: 1.0,
            blocked: &blocked,
            front: Vec2::default(),
        };
        let mut routes = vec![
            Route {
                tee: Vec2::new(100.0, 100.0),
                dogleg: None,
                bend: None,
                green: Vec2::new(100.0, 400.0),
                par: 4,
            },
            Route {
                tee: Vec2::new(130.0, 420.0),
                dogleg: None,
                bend: None,
                green: Vec2::new(130.0, 120.0),
                par: 4,
            },
        ];
        let before = cost::total(&site, &routes).total();
        refine(&site, &mut routes, &mut Rng::new(3), 1500);
        let after = cost::total(&site, &routes).total();
        assert!(after <= before + 1.0, "{before} -> {after}");
        assert!(routes[0].green.distance(routes[1].green) > 1.0);
    }
}
