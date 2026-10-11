//! Working out the swing: the power at which a club sends the ball a
//! given distance *on this ground*, with its slope, the roll after the
//! carry, the lie and the wind. The meter's tick sits at that power, so a
//! click on it sends the ball to the aim marker, uphill or down. It is the
//! shot simulator run a few times, nothing more.

use crate::math::{Vec2, Vec3};

use super::ball::Wind;
use super::clubs::Club;
use super::shot::play_out;
use super::surface::Lie;
use super::terrain::{Filtered, Terrain};

/// The launch a swing makes: the club's, scaled by the lie. Shared by the
/// strike and by the solver, so what the tick promises is what happens.
pub fn launch(
    club: &Club,
    at: Vec3,
    heading: Vec2,
    power: f32,
    sidespin: f32,
    lie: &Lie,
) -> super::ball::Launch {
    let mut l = club.launch(at, heading, power * lie.power, sidespin);
    l.backspin *= lie.spin;
    l
}

/// How far along `heading` the ball ends up from `at` with a swing of
/// `power` (a share of a full swing).
fn reach(
    club: &Club,
    terrain: &dyn Terrain,
    at: Vec3,
    heading: Vec2,
    power: f32,
    wind: Wind,
    lie: &Lie,
) -> f32 {
    let l = launch(club, at, heading, power, 0.0, lie);
    let ideal = Filtered {
        inner: terrain,
        trees: false,
    };
    let flight = play_out(&l, wind, &ideal);
    flight.samples.last().map_or(0.0, |end| {
        (end.position.ground() - at.ground()).dot(heading.normalized())
    })
}

/// The cursor height (a share of a full swing, at most 1) that sends the
/// ball `distance` metres from `at` along `heading`. A distance that is
/// out of the club's reach is a full swing.
pub fn power_for(
    club: &Club,
    terrain: &dyn Terrain,
    at: Vec3,
    heading: Vec2,
    distance: f32,
    wind: Wind,
    lie: &Lie,
) -> f32 {
    let full = |p: f32| reach(club, terrain, at, heading, p, wind, lie);
    if full(1.0) <= distance {
        return 1.0;
    }
    // The cursor is a share of the swing before the lie takes its part.
    let (mut low, mut high) = (0.0f32, 1.0f32);
    for _ in 0..11 {
        let mid = 0.5 * (low + high);
        if full(mid) < distance {
            low = mid;
        } else {
            high = mid;
        }
    }
    0.5 * (low + high)
}
