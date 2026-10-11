//! Playing golf (docs/golf-gameplay-plan.md): the ball's flight, the
//! clubs that launch it, and the state of the hole being played. Pure
//! data and arithmetic, so it runs under `cargo test` with no window and a
//! shot is deterministic.

pub mod ball;
pub mod caddy;
pub mod cam;
pub mod clubs;
pub mod green;
pub mod meter;
pub mod plan;
pub mod shot;
pub mod solve;
pub mod surface;
pub mod terrain;
mod turn;

use crate::course::{Course, Hole, Material};
use crate::math::{Vec2, Vec3};
use crate::render::overlay::AimMark;
use crate::render::{Bake, Camera};

pub use ball::{simulate, Event, Flight, Launch, Sample, Wind};
pub use clubs::{Club, BAG, CLUBS, PUTTER_INDEX};
pub use meter::Meter;
pub use turn::{Phase, PlayEvent, MAX_STROKES};

use cam::address::{Address, Style};
use cam::{Director, Obstacles};
use terrain::{CourseTerrain, Terrain};

/// The ball rests this far above the ground, metres (its radius, and a
/// little, so it is drawn on top of the grass).
const BALL_REST: f32 = 0.03;
/// The aim marker's ring is about this many screen pixels at 1280 wide.
const RING_PER_METRE: f32 = 0.012;

/// The hole being played: where the ball lies, which way and with what
/// club it is about to be hit, and what has happened so far.
#[derive(Clone, Debug, PartialEq)]
pub struct Play {
    pub hole: usize,
    /// Where the ball rests; the next stroke is played from here.
    pub ball: Vec3,
    /// Unit ground direction of the aim.
    pub heading: Vec2,
    /// Index into [`BAG`].
    pub club: usize,
    /// How far the shot is meant to go, metres: where the aim marker
    /// stands. A full swing of the club is the most it can be; a chip
    /// is a short club to a near target.
    pub distance: f32,
    /// Strokes taken on this hole, penalties included.
    pub stroke: u32,
    pub phase: Phase,
    /// The shot in the air, and the camera that follows it.
    pub flight: Option<Flight>,
    pub director: Option<Director>,
    /// Where the last stroke was played from (a penalty goes back there).
    pub from: Vec3,
    /// Strokes taken on each hole of the round, once holed.
    pub scores: Vec<Option<u32>>,
    /// Where the meter's tick stands for the swing in progress.
    pub swing_tick: f32,
    /// How many times in a row this lie has cost a penalty (the caddy
    /// plays it safer each time).
    pub retries: u32,
    /// What the ball's lie does to a swing (power it takes off).
    pub lie: surface::Lie,
    /// What to tell the player, and for how many more seconds.
    pub message: Option<(String, f32)>,
    /// What happened since the host last asked.
    pub events: Vec<PlayEvent>,
    pub wind: Wind,
    /// Seeds the lie's scatter, so a replayed round is the same round.
    pub seed: u64,
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
            phase: Phase::Address,
            flight: None,
            director: None,
            from: ball,
            swing_tick: 1.0,
            retries: 0,
            lie: surface::Lie {
                power: 1.0,
                spin: 1.0,
                scatter: 0.0,
            },
            scores: vec![None; holes.len()],
            message: None,
            events: Vec::new(),
            wind: Wind::default(),
            seed: 0,
        };
        play.club = caddy::recommend(to_target, h.par >= 4);
        play.distance = to_target.min(play.carry());
        play
    }

    pub fn club(&self) -> &'static Club {
        &BAG[self.club]
    }

    /// The club's full-swing carry, metres.
    pub fn carry(&self) -> f32 {
        self.club().carry_m
    }

    /// The share of a full swing that sends the ball `distance` from where
    /// it lies, slope, roll, lie and wind included: where the tick on the
    /// meter stands.
    pub fn target_power(&self, course: &Course, bake: &Bake) -> f32 {
        let terrain = CourseTerrain::new(course, bake, self.hole);
        solve::power_for(
            self.club(),
            &terrain,
            self.ball,
            self.heading,
            self.distance,
            self.wind,
            &self.lie,
        )
    }

    /// Turns the aim by `radians` (positive: clockwise seen from above).
    pub fn rotate(&mut self, radians: f32) {
        self.heading = rotated(self.heading, radians);
    }

    /// A longer (`-1`) or shorter (`+1`) club; stays within the bag.
    pub fn step_club(&mut self, towards_shorter: isize) {
        let next = self.club as isize + towards_shorter;
        self.club = next.clamp(0, BAG.len() as isize - 1) as usize;
        // A longer club goes for its full reach (carry and a little
        // roll); a shorter one never lengthens the shot.
        let reach = self.carry() * 1.1;
        self.distance = if towards_shorter > 0 {
            reach.min(self.distance)
        } else {
            reach
        };
    }

    /// Chooses the club and the aim for the ball where it lies: the
    /// putter on the green, a wedge out of sand, otherwise the caddy's.
    pub fn set_up(&mut self, course: &Course, bake: &Bake) {
        let Some(hole) = course.holes.get(self.hole) else {
            return;
        };
        let terrain = CourseTerrain::new(course, bake, self.hole);
        let at = self.ball.ground();
        let material = terrain.material(at.x, at.y);
        let to_pin = at.distance(hole.pin);
        self.set_lie(material);
        if material == Material::Green || (material == Material::Fringe && to_pin < 12.0) {
            self.club = PUTTER_INDEX;
            self.heading = (hole.pin - at).normalized();
            self.distance = to_pin.clamp(0.5, 60.0);
            self.set_lie(material);
            return;
        }
        let tee = self.stroke == 0 && hole.par >= 4;
        let plan = plan::plan(hole, &terrain, self.ball, material, self.wind, tee);
        self.heading = plan.heading;
        self.club = plan.club;
        self.distance = plan.distance;
        self.set_lie(material);
    }

    fn set_lie(&mut self, material: Material) {
        self.lie = surface::lie(material, self.club());
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

    fn style(&self) -> Style {
        if self.club().is_putter() {
            Style::Putt
        } else {
            Style::Full
        }
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
            self.style(),
        )
    }

    /// The camera for what is happening: the address camera, or the
    /// director's while a shot is in the air and after it.
    pub fn camera(&self, bake: &Bake, aspect: f32) -> Camera {
        match (&self.phase, &self.director) {
            (Phase::Address | Phase::Swing(_), _) | (_, None) => {
                self.address_camera(bake, aspect).camera
            }
            (_, Some(director)) => director.camera,
        }
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
        let radius = if self.club().is_putter() {
            0.25
        } else {
            (length * RING_PER_METRE).max(0.6)
        };
        AimMark {
            dots,
            target,
            radius,
        }
    }
}

/// `v` turned `radians` clockwise seen from above (x east, y south).
pub fn rotated(v: Vec2, radians: f32) -> Vec2 {
    let (s, c) = radians.sin_cos();
    Vec2::new(v.x * c - v.y * s, v.x * s + v.y * c).normalized()
}
