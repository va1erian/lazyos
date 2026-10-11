//! A hole as a sequence of turns: address, swing, flight, rest (or the
//! cup, or a penalty), and the next address. The phases and what moves
//! between them; the physics is `shot`, the cameras are `cam`.

use crate::course::{Course, Material};
use crate::math::Vec3;
use crate::render::Bake;

use super::ball::{Event, Flight};
use super::cam::{Director, Obstacles};
use super::meter::{self, Meter};
use super::shot;
use super::solve;
use super::surface;
use super::terrain::{CourseTerrain, Terrain};
use super::{rotated, Play};

/// A hole is picked up after this many strokes (a beginner's mercy, and a
/// bound on every hole).
pub const MAX_STROKES: u32 = 10;
const REST_HOLD: f32 = 1.3;
const PENALTY_HOLD: f32 = 2.4;
const HOLED_HOLD: f32 = 3.5;
const MESSAGE_TIME: f32 = 3.5;

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// Lining the shot up: aim and club.
    Address,
    /// The meter is running.
    Swing(Meter),
    /// The ball is moving: `t` seconds after impact.
    Flight { t: f32, landed: bool },
    /// The ball has stopped (or gone in the water); the shot is held a
    /// moment before the next address.
    Rest { wait: f32 },
    /// The ball is in the cup.
    Holed { wait: f32 },
}

/// What happened, for the host to report (serial evidence, a scorecard).
#[derive(Clone, Debug, PartialEq)]
pub enum PlayEvent {
    Shot {
        hole: u8,
        stroke: u32,
        club: &'static str,
        power: f32,
    },
    Landed {
        hole: u8,
        stroke: u32,
        material: Material,
        carry: f32,
    },
    Penalty {
        hole: u8,
        stroke: u32,
        kind: &'static str,
    },
    Holed {
        hole: u8,
        strokes: u32,
        par: u8,
    },
    /// The hole was given up at [`MAX_STROKES`] without holing out.
    PickedUp {
        hole: u8,
        strokes: u32,
        par: u8,
    },
}

/// The name of a score against par.
pub fn score_name(strokes: u32, par: u8) -> &'static str {
    if strokes == 1 {
        return "Hole in one!";
    }
    match strokes as i32 - i32::from(par) {
        i32::MIN..=-3 => "Albatross!",
        -2 => "Eagle!",
        -1 => "Birdie",
        0 => "Par",
        1 => "Bogey",
        2 => "Double bogey",
        _ => "Over par",
    }
}

/// A deterministic number in -1..1 for a shot.
fn noise(seed: u64, hole: usize, stroke: u32, salt: u64) -> f32 {
    let mut x = seed
        .wrapping_add((hole as u64) << 32)
        .wrapping_add(u64::from(stroke) << 8)
        .wrapping_add(salt)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x >> 29;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 32;
    (x & 0xFF_FFFF) as f32 / 8_388_608.0 - 1.0
}

impl Play {
    /// The player's click: starts the swing, sets the power, sets the
    /// accuracy; during a shot it skips ahead to the next address.
    pub fn click(&mut self, course: &Course, bake: &Bake, aspect: f32) {
        match &mut self.phase {
            Phase::Address => {
                self.swing_tick = self.target_power(course, bake);
                let mut m = Meter::Ready;
                m.click();
                self.phase = Phase::Swing(m);
            }
            Phase::Swing(m) => {
                m.click();
                if let Some((power, error)) = m.result() {
                    self.hit(course, bake, aspect, power, error);
                }
            }
            _ => self.skip(course, bake),
        }
    }

    /// Backs out of a swing in progress.
    pub fn cancel(&mut self) {
        if matches!(self.phase, Phase::Swing(_)) {
            self.phase = Phase::Address;
        }
    }

    /// Strikes the ball with `power` (a share of a full swing) and the
    /// accuracy `error` of the meter (0: straight). The meter calls this;
    /// so do the tests and, later, the computer player.
    pub fn hit(&mut self, course: &Course, bake: &Bake, aspect: f32, power: f32, error: f32) {
        let terrain = CourseTerrain::new(course, bake, self.hole);
        let at = self.ball;
        let club = *self.club();
        let lie = surface::lie(terrain.material(at.x, at.z), &club);
        self.stroke += 1;
        let hole = self.hole;
        let (off, spin) = meter::miss(error);
        let scatter = lie.scatter * noise(self.seed, hole, self.stroke, 1);
        let heading = rotated(self.heading, off + scatter);
        let side = spin + lie.scatter * 1500.0 * noise(self.seed, hole, self.stroke, 2);
        let launch = solve::launch(&club, at, heading, power, side, &lie);

        let flight = shot::play_out(&launch, self.wind, &terrain);
        let mut address = self.address_camera(bake, aspect).camera;
        address.pitch += self.look;
        let pin = course.holes.get(hole).map(|h| h.pin);
        self.director = Some(Director::new(
            &address,
            &flight,
            &Obstacles { bake },
            pin,
            aspect,
        ));
        self.flight = Some(flight);
        self.from = at;
        self.phase = Phase::Flight {
            t: 0.0,
            landed: false,
        };
        self.events.push(super::PlayEvent::Shot {
            hole: hole as u8 + 1,
            stroke: self.stroke,
            club: club.name,
            power: power * lie.power,
        });
    }

    /// `dt` seconds pass. Returns whether anything the HUD or the picture
    /// shows has changed.
    pub fn tick(&mut self, dt: f32, course: &Course, bake: &Bake, aspect: f32) -> bool {
        if let Some((_, left)) = &mut self.message {
            *left -= dt;
            if *left <= 0.0 {
                self.message = None;
            }
        }
        match self.phase.clone() {
            Phase::Address => false,
            Phase::Swing(mut m) => {
                m.tick(dt);
                if let Some((power, error)) = m.result() {
                    self.hit(course, bake, aspect, power, error);
                } else {
                    self.phase = Phase::Swing(m);
                }
                true
            }
            Phase::Flight { t, landed } => {
                self.fly(t + dt, landed, dt, course, bake);
                true
            }
            Phase::Rest { wait } => {
                if wait - dt <= 0.0 {
                    self.next_stroke(course, bake);
                    true
                } else {
                    self.phase = Phase::Rest { wait: wait - dt };
                    false
                }
            }
            Phase::Holed { wait } => {
                if wait - dt <= 0.0 {
                    self.next_hole(course, bake);
                    true
                } else {
                    self.phase = Phase::Holed { wait: wait - dt };
                    false
                }
            }
        }
    }

    /// Plays the flight to `t`.
    fn fly(&mut self, t: f32, landed: bool, dt: f32, course: &Course, bake: &Bake) {
        let Some(flight) = self.flight.as_ref() else {
            self.phase = Phase::Address;
            return;
        };
        let t = t.min(flight.duration());
        let (ball, velocity) = (flight.at(t), flight.velocity_at(t));
        let land = flight.event(Event::Land).map(|s| (s.t, s.position));
        let over = t >= flight.duration();
        let mut landed = landed;
        if let (false, Some((at, p))) = (landed, land) {
            if t >= at {
                landed = true;
                let m = CourseTerrain::new(course, bake, self.hole).material(p.x, p.z);
                let carry = flight
                    .samples
                    .first()
                    .map_or(0.0, |s| s.position.ground().distance(p.ground()));
                self.message = Some((format!("{}  {carry:.0} m", m.name()), MESSAGE_TIME));
                self.events.push(PlayEvent::Landed {
                    hole: self.hole as u8 + 1,
                    stroke: self.stroke,
                    material: m,
                    carry,
                });
            }
        }
        if let Some(director) = self.director.as_mut() {
            director.update(dt, t, ball, velocity);
        }
        if over {
            self.finish(course, bake);
        } else {
            self.phase = Phase::Flight { t, landed };
        }
    }

    /// The shot is over: score it and decide what comes next.
    fn finish(&mut self, course: &Course, bake: &Bake) {
        let Some(flight) = self.flight.as_ref() else {
            return;
        };
        let par = course.holes.get(self.hole).map_or(4, |h| h.par);
        let hole = self.hole as u8 + 1;
        let last = flight.samples.last().map_or(self.ball, |s| s.position);
        match flight.end() {
            Event::Holed => {
                self.scores[self.hole] = Some(self.stroke);
                let name = score_name(self.stroke, par);
                self.message = Some((format!("{name}  ({} strokes)", self.stroke), HOLED_HOLD));
                self.events.push(PlayEvent::Holed {
                    hole,
                    strokes: self.stroke,
                    par,
                });
                self.phase = Phase::Holed { wait: HOLED_HOLD };
                return;
            }
            end @ (Event::Splash | Event::OutOfBounds) => {
                // Stroke and distance: the penalty stroke, and play again
                // from where it was hit.
                self.stroke += 1;
                let kind = if end == Event::Splash {
                    "water"
                } else {
                    "out of bounds"
                };
                self.message = Some((format!("Ball in {kind}: +1 stroke"), PENALTY_HOLD));
                self.events.push(PlayEvent::Penalty {
                    hole,
                    stroke: self.stroke,
                    kind,
                });
                self.phase = Phase::Rest { wait: PENALTY_HOLD };
                return;
            }
            _ => {
                self.ball = last;
                let pin = course
                    .holes
                    .get(self.hole)
                    .map_or(0.0, |h| h.pin.distance(last.ground()));
                let m = CourseTerrain::new(course, bake, self.hole).material(last.x, last.z);
                self.message = Some((format!("{}  {pin:.0} m to the pin", m.name()), MESSAGE_TIME));
            }
        }
        self.phase = Phase::Rest { wait: REST_HOLD };
    }

    /// After the hold: back to address, or on to the next hole when the
    /// hole is done by pick-up.
    fn next_stroke(&mut self, course: &Course, bake: &Bake) {
        let after_penalty = self
            .flight
            .as_ref()
            .is_some_and(|f| matches!(f.end(), Event::Splash | Event::OutOfBounds));
        if after_penalty {
            self.ball = self.from;
            self.retries += 1;
        } else {
            self.retries = 0;
        }
        if self.stroke >= MAX_STROKES {
            self.scores[self.hole] = Some(self.stroke);
            self.message = Some(("Pick up".into(), MESSAGE_TIME));
            self.events.push(PlayEvent::PickedUp {
                hole: self.hole as u8 + 1,
                strokes: self.stroke,
                par: course.holes.get(self.hole).map_or(4, |h| h.par),
            });
            self.phase = Phase::Holed { wait: 1.5 };
            return;
        }
        self.flight = None;
        self.director = None;
        self.phase = Phase::Address;
        self.set_up(course, bake);
        if after_penalty {
            self.play_safer();
        }
    }

    /// After a penalty: a shorter shot, a little to the other side, more
    /// each time, so a retry is not the same shot into the same water.
    fn play_safer(&mut self) {
        let n = self.retries as f32;
        self.distance = (self.distance * (1.0 - 0.12 * n)).max(25.0);
        let side = if self.retries % 2 == 1 { 1.0 } else { -1.0 };
        self.rotate(side * 0.05 * n);
    }

    /// The next hole's tee (the first again after the eighteenth, with a
    /// fresh card).
    pub fn next_hole(&mut self, course: &Course, bake: &Bake) {
        let n = course.holes.len().max(1);
        let next = (self.hole + 1) % n;
        self.start_hole(course, bake, next);
        if next == 0 {
            self.scores = vec![None; n];
        }
    }

    /// Starts `hole` from its tee, keeping the card.
    pub fn start_hole(&mut self, course: &Course, bake: &Bake, hole: usize) {
        let scores = std::mem::take(&mut self.scores);
        // What has happened and not yet been read goes with the player.
        let events = std::mem::take(&mut self.events);
        let (wind, seed) = (self.wind, self.seed);
        *self = Play::tee_off(&course.holes, bake, hole);
        self.scores = scores;
        self.events = events;
        self.wind = wind;
        self.seed = seed;
        self.set_up(course, bake);
    }

    /// Jumps to the end of what is happening: the shot's finish, the rest,
    /// the next address.
    pub fn skip(&mut self, course: &Course, bake: &Bake) {
        for _ in 0..4 {
            match self.phase {
                Phase::Flight { landed, .. } => {
                    // Skipping past the landing still reports it.
                    if let Some(f) = &self.flight {
                        let end = f.duration();
                        self.fly(end, landed, 0.0, course, bake);
                    }
                }
                Phase::Rest { .. } => self.next_stroke(course, bake),
                Phase::Holed { .. } => self.next_hole(course, bake),
                _ => return,
            }
        }
    }

    /// Where to draw the ball, or `None` when it is out of sight (in the
    /// water).
    pub fn ball_shown(&self) -> Option<Vec3> {
        let flight: Option<&Flight> = self.flight.as_ref();
        match (&self.phase, flight) {
            (Phase::Flight { t, .. }, Some(f)) => Some(f.at(*t)),
            (Phase::Rest { .. }, Some(f)) if f.end() == Event::Splash => None,
            (Phase::Rest { .. } | Phase::Holed { .. }, Some(f)) if f.end() == Event::Holed => {
                Some(f.at(f.duration()))
            }
            _ => Some(self.ball),
        }
    }

    /// Whether the player is in a stroke (the meter or a flight): the
    /// aim and club are not theirs to change.
    pub fn in_stroke(&self) -> bool {
        !matches!(self.phase, Phase::Address)
    }
}
