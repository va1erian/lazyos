//! Bending a hole around a landmark. A drawn corridor that runs into a hill,
//! a crag or an old wood gets a dogleg corner beside the obstacle instead,
//! keeping its length: the land shapes the hole, as it does on real courses.

use crate::math::{polyline_at, Vec2};

use super::cost::{self, Site};
use super::Route;

/// Sideways offsets tried for the corner, metres, nearest first.
const OFFSETS: [f32; 6] = [45.0, 65.0, 85.0, 110.0, 135.0, 160.0];

/// `route` bent around whatever blocks it, or `None` if no single corner
/// clears it. A route that is already clear comes back unchanged.
pub fn around(site: &Site, route: Route) -> Option<Route> {
    let line = route.line();
    if cost::is_clear(site, &line) {
        return Some(route);
    }
    let length = route.length();
    let (start, end) = blocked_span(site, &line)?;
    // Beside the middle of the obstacle first, then beside either end (or
    // beside the old corner, if the hole had one).
    let spots: Vec<(Vec2, Vec2)> = match route.dogleg {
        Some(corner) => vec![(corner, (corner - route.tee).normalized())],
        None => [0.5, 0.2, 0.8]
            .iter()
            .map(|f| polyline_at(&line, start + (end - start) * f))
            .collect(),
    };
    for offset in OFFSETS {
        for &(at, dir) in &spots {
            for side in [1.0, -1.0] {
                if let Some(bent) = bend(site, &route, length, at + dir.perp() * (side * offset)) {
                    return Some(bent);
                }
            }
        }
    }
    None
}

/// `route` with its corner at `corner`, the second leg aimed at the old
/// green and cut to the old length, if that plays and clears everything.
fn bend(site: &Site, route: &Route, length: f32, corner: Vec2) -> Option<Route> {
    let first = route.tee.distance(corner);
    if first < 80.0 || first > length - 60.0 {
        return None;
    }
    let toward = (route.green - corner).normalized();
    if (corner - route.tee).normalized().dot(toward) < 0.66 {
        return None;
    }
    let green = corner + toward * (length - first);
    let bent = Route {
        dogleg: Some(corner),
        bend: None,
        green,
        ..*route
    };
    (cost::is_clear(site, &bent.line()) && !blocked_spot(site, green)).then_some(bent)
}

/// The distances along `line` of the first and last blocked samples.
fn blocked_span(site: &Site, line: &[Vec2]) -> Option<(f32, f32)> {
    let length = crate::math::polyline_length(line);
    let mut span: Option<(f32, f32)> = None;
    let mut s = 0.0;
    while s <= length {
        let (p, dir) = polyline_at(line, s);
        let sides = [-cost::PLAY_CLEARANCE, 0.0, cost::PLAY_CLEARANCE];
        if sides
            .iter()
            .any(|&o| blocked_spot(site, p + dir.perp() * o))
        {
            span = Some(span.map_or((s, s), |(a, _)| (a, s)));
        }
        s += 8.0;
    }
    span
}

fn blocked_spot(site: &Site, p: Vec2) -> bool {
    site.blocked.width > 0 && site.blocked.at(p.x, p.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::Field;

    #[test]
    fn a_hole_bends_around_a_hill_in_its_way() {
        let elevation = Field::new(600, 600, 0.0f32);
        let water = Field::new(600, 600, false);
        let mut blocked = Field::new(600, 600, false);
        // A hill straddling the line from (100, 300) to (480, 300).
        for y in 270..330 {
            for x in 260..320 {
                blocked.set(x, y, true);
            }
        }
        let site = Site {
            elevation: &elevation,
            water: &water,
            size: 600.0,
            clubhouse: Vec2::new(300.0, 500.0),
            buffer: 60.0,
            max_slope: 0.1,
            relief: 1.0,
            blocked: &blocked,
            front: Vec2::default(),
        };
        let straight = Route {
            tee: Vec2::new(100.0, 300.0),
            dogleg: None,
            bend: None,
            green: Vec2::new(480.0, 300.0),
            par: 4,
        };
        assert!(cost::blocked(&site, &straight.line()) > 0.0);
        let bent = around(&site, straight).expect("a way round");
        assert!(bent.dogleg.is_some());
        assert_eq!(cost::blocked(&site, &bent.line()), 0.0);
        assert!((bent.length() - straight.length()).abs() < 1.0);
    }
}
