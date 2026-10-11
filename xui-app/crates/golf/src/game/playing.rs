//! The game's side of playing a hole: the keys that aim and swing, the
//! play state's tick, what the overlay shows of it, and what it reports.

use crate::fly::{Action, Held};
use crate::play::{green, Phase, PlayEvent, PUTTER_INDEX};
use crate::render::BallMark;

use super::{Report, Running};

/// How fast a held key turns the aim, radians per second.
const AIM_RATE: f32 = 0.3;

impl Running {
    /// Height over width of the picture, for the shot camera.
    fn aspect(&self) -> f32 {
        self.size.1 as f32 / self.size.0.max(1) as f32
    }

    /// Whether the meter is moving (the HUD needs repainting, the 3D view
    /// does not).
    pub fn swinging(&self) -> bool {
        !self.free && matches!(self.play.phase, Phase::Swing(_))
    }

    /// Shows the ball, the aim and (when putting) the green's slope grid
    /// for the current play state, and redraws.
    pub fn sync_play(&mut self) {
        let bake = &self.scene.bake;
        self.overlay.ball = self.play.ball_shown().map(|position| BallMark {
            position,
            ground: bake.height_at(position.x, position.z),
        });
        let lining_up = matches!(self.play.phase, Phase::Address | Phase::Swing(_));
        self.overlay.aim = lining_up.then(|| self.play.aim_mark(bake));
        let putting = lining_up && self.play.club == PUTTER_INDEX;
        if putting {
            let hole = self.play.hole;
            if self.grid.as_ref().is_none_or(|(h, _)| *h != hole) {
                let arrows = self
                    .scene
                    .course
                    .holes
                    .get(hole)
                    .map(|h| green::grid(h, bake))
                    .unwrap_or_default();
                self.grid = Some((hole, arrows));
            }
            self.overlay.grid = self
                .grid
                .as_ref()
                .map(|(_, g)| g.clone())
                .unwrap_or_default();
        } else {
            self.overlay.grid.clear();
        }
        self.moved();
    }

    /// Plays hole `n` from its tee.
    pub fn start_hole(&mut self, n: usize) {
        let scene = &self.scene;
        self.play.start_hole(&scene.course, &scene.bake, n);
        self.free = false;
        self.sync_play();
    }

    /// Switches between the shot camera and the free one.
    pub fn toggle_free(&mut self) {
        self.free = !self.free;
        if self.free {
            // Fly on from where the shot camera was.
            self.flyer.camera = self.camera;
            self.overlay.aim = None;
            self.overlay.grid.clear();
            self.moved();
        } else {
            self.sync_play();
        }
    }

    /// The player's click: swing, set power, set accuracy; during a shot
    /// it skips ahead.
    pub fn click(&mut self) {
        let aspect = self.aspect();
        let scene = &self.scene;
        self.play.click(&scene.course, &scene.bake, aspect);
        self.sync_play();
    }

    /// Backs out of a swing in progress.
    pub fn cancel_swing(&mut self) {
        self.play.cancel();
        self.sync_play();
    }

    /// One tick of the hole being played.
    pub(super) fn play_step(&mut self, held: Held, dt: f32, reports: &mut Vec<Report>) {
        if !self.play.in_stroke() {
            self.aim_tick(held, dt);
        }
        let aspect = self.aspect();
        let scene = &self.scene;
        let changed = self.play.tick(dt, &scene.course, &scene.bake, aspect);
        if changed {
            self.sync_play();
        }
        for event in self.play.events.drain(..) {
            reports.push(match event {
                PlayEvent::Shot {
                    hole,
                    stroke,
                    club,
                    power,
                } => Report::Shot {
                    hole,
                    stroke,
                    club: club.into(),
                    power,
                },
                PlayEvent::Landed {
                    hole,
                    stroke,
                    material,
                    carry,
                } => Report::Landed {
                    hole,
                    stroke,
                    material: material.name().into(),
                    carry,
                },
                PlayEvent::Penalty { hole, stroke, kind } => Report::Penalty {
                    hole,
                    stroke,
                    kind: kind.into(),
                },
                PlayEvent::Holed { hole, strokes, par } => Report::Holed { hole, strokes, par },
            });
        }
    }

    /// Held turn keys rotate the aim (Shift for a fast turn).
    fn aim_tick(&mut self, held: Held, dt: f32) {
        let turn = |plus: Action, minus: Action| {
            f32::from(u8::from(held.has(plus))) - f32::from(u8::from(held.has(minus)))
        };
        let axis = turn(Action::TurnRight, Action::TurnLeft) + turn(Action::Right, Action::Left);
        if axis != 0.0 {
            let fast = if held.has(Action::Fast) { 4.0 } else { 1.0 };
            self.play
                .rotate(axis.clamp(-1.0, 1.0) * AIM_RATE * fast * dt);
            self.sync_play();
        }
    }
}
