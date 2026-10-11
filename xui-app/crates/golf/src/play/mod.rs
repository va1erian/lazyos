//! Playing golf (docs/golf-gameplay-plan.md): the ball's flight, the
//! clubs that launch it, and the state of the hole being played. Pure
//! data and arithmetic, so it runs under `cargo test` with no window and a
//! shot is deterministic.

pub mod ball;
pub mod caddy;
pub mod cam;
pub mod clubs;

use crate::course::Hole;
use crate::math::{Vec2, Vec3};
use crate::render::overlay::AimMark;
use crate::render::{Bake, Camera};

pub use ball::{simulate, Event, Flight, Launch, Sample, Wind};
pub use clubs::{Club, CLUBS};

use cam::{Address, Obstacles};

/// The ball rests this far above the ground, metres (its radius, and a
/// little, so it is drawn on top of the grass).
const BALL_REST: f32 = 0.03;
/// The aim marker's ring is about this many screen pixels at 1280 wide.
const RING_PER_METRE: f32 = 0.012;

/// The hole being played: where the ball lies, which way and with what
/// club it is about to be hit.
#[derive(Clone, Debug, PartialEq)]
pub struct Play {
    pub hole: usize,
    pub ball: Vec3,
    /// Unit ground direction of the aim.
    pub heading: Vec2,
    /// Index into [`CLUBS`].
    pub club: usize,
    /// How far the shot is meant to go, metres: where the aim marker
    /// stands. A full swing of the club is the most it can be; a chip
    /// is a short club to a near target.
    pub distance: f32,
    /// Strokes taken on this hole.
    pub stroke: u32,
}

impl Play {
    /// The first stroke of `hole`, from the middle tees.
    pub fn tee_off(holes: &[Hole], bake: &Bake, hole: usize) -> Play {
        let hole = hole.min(holes.len().saturating_sub(1));
        let h = &holes[hole];
        let tee = h
            .tees
            .get(1)
            .or(h.tees.first())
            .map_or(Vec2::default(), |t| t.position);
        let (heading, to_target) = caddy::default_aim(h, tee);
        let ball = Vec3::new(tee.x, bake.height_at(tee.x, tee.y) + BALL_REST, tee.y);
        let mut play = Play {
            hole,
            ball,
            heading,
            club: 0,
            distance: to_target,
            stroke: 0,
        };
        play.club = caddy::recommend(to_target, h.par >= 4);
        play.distance = to_target.min(play.carry());
        play
    }

    pub fn club(&self) -> &'static Club {
        &CLUBS[self.club]
    }

    /// The club's full-swing carry, metres.
    pub fn carry(&self) -> f32 {
        self.club().carry_m
    }

    /// Turns the aim by `radians` (positive: clockwise seen from above).
    pub fn rotate(&mut self, radians: f32) {
        let (s, c) = radians.sin_cos();
        let h = self.heading;
        // A turn toward the player's right: right of (x, z) is (-z, x).
        self.heading = Vec2::new(h.x * c - h.y * s, h.x * s + h.y * c).normalized();
    }

    /// A longer (`-1`) or shorter (`+1`) club; stays within the bag.
    pub fn step_club(&mut self, towards_shorter: isize) {
        let next = self.club as isize + towards_shorter;
        self.club = next.clamp(0, CLUBS.len() as isize - 1) as usize;
        // A longer club goes for its full carry; a shorter one never
        // lengthens the shot.
        self.distance = if towards_shorter > 0 {
            self.carry().min(self.distance)
        } else {
            self.carry()
        };
    }

    /// Metres from the ball to the pin.
    pub fn distance_to_pin(&self, holes: &[Hole]) -> f32 {
        holes
            .get(self.hole)
            .map_or(0.0, |h| self.ball.ground().distance(h.pin))
    }

    /// Where the shot is meant to land: the marker's centre, on the ground.
    pub fn aim_point(&self, bake: &Bake) -> Vec3 {
        let p = self.ball.ground() + self.heading * self.distance;
        Vec3::new(p.x, bake.height_at(p.x, p.y), p.y)
    }

    /// The camera that lines the shot up in a picture `aspect` (height over
    /// width) tall.
    pub fn address_camera(&self, bake: &Bake, aspect: f32) -> Address {
        cam::address(
            &Obstacles { bake },
            self.ball,
            self.heading,
            self.distance,
            aspect,
        )
    }

    pub fn camera(&self, bake: &Bake, aspect: f32) -> Camera {
        self.address_camera(bake, aspect).camera
    }

    /// The ground markings for the aim: dots from the ball to a ring.
    pub fn aim_mark(&self, bake: &Bake) -> AimMark {
        let target = self.aim_point(bake);
        let length = self.distance;
        let dots = (1..)
            .map(|i| i as f32 * 4.0)
            .take_while(|d| *d < length - 3.0)
            .map(|d| {
                let p = self.ball.ground() + self.heading * d;
                Vec3::new(p.x, bake.height_at(p.x, p.y) + 0.04, p.y)
            })
            .collect();
        AimMark {
            dots,
            target,
            radius: (length * RING_PER_METRE).max(0.6),
        }
    }
}
