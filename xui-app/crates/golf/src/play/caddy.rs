//! The caddy: which club for a distance, and where to aim by default.

use crate::course::Hole;
use crate::math::{polyline_at, polyline_distance, polyline_length, Vec2};

use super::clubs::CLUBS;

/// A tee shot is a driver only when it goes at least this far.
const DRIVER_MIN: f32 = 150.0;

/// The club to start from: the shortest whose full carry gets to
/// `distance`, or the longest if none does. A tee shot on a par 4 or 5
/// is a driver whatever the distance to the aim point, as a caddy would
/// say.
pub fn recommend(distance: f32, tee_shot_on_long_hole: bool) -> usize {
    if tee_shot_on_long_hole && distance >= DRIVER_MIN {
        return 0;
    }
    CLUBS
        .iter()
        .rposition(|c| c.carry_m >= distance)
        .unwrap_or(0)
}

/// Where to start aiming from `ball`: the direction and the distance.
/// At the pin when it is in reach of the longest club, otherwise down
/// the line of play, a long shot's length ahead.
pub fn default_aim(hole: &Hole, ball: Vec2) -> (Vec2, f32) {
    let reach = CLUBS[0].carry_m;
    let to_pin = ball.distance(hole.pin);
    if to_pin <= reach {
        return ((hole.pin - ball).normalized(), to_pin);
    }
    let (_, along) = polyline_distance(ball, &hole.centerline);
    let ahead = (along + reach * 0.9).min(polyline_length(&hole.centerline));
    let (target, _) = polyline_at(&hole.centerline, ahead);
    (
        (target - ball).normalized(),
        ball.distance(target).min(reach),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shortest_club_that_gets_there() {
        assert_eq!(CLUBS[recommend(95.0, false)].name, "Pitching Wedge");
        assert_eq!(CLUBS[recommend(96.0, false)].name, "9 Iron");
        assert_eq!(CLUBS[recommend(10.0, false)].name, "Sand Wedge");
        assert_eq!(recommend(500.0, false), 0, "out of reach: the longest");
        assert_eq!(recommend(190.0, true), 0, "a driver off a long tee");
        assert_eq!(
            CLUBS[recommend(120.0, true)].name,
            "8 Iron",
            "but not for a short hole"
        );
    }
}
