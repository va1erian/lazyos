//! The address camera: where the player stands to line up a shot
//! (docs/golf-gameplay-plan.md, 4.3). Behind the ball on the line to the
//! aim point, a little to the right and above, the ball low in the frame
//! and the aim point in the upper part of it.
//!
//! The placement is a search, not a formula: a preferred spot, then spots
//! lifted and shifted until the eye is clear of the ground and the trees
//! and has an unobstructed line to the ball; the pitch is then solved so
//! the ball and the aim point are both inside the picture (a constraint,
//! not a hope, so tests can hold it to that for every hole).

use crate::fly::yaw_of;
use crate::math::{lerp, smoothstep, Vec2, Vec3};
use crate::render::Camera;

use super::obstacles::Obstacles;

/// The eye is never lower than this above the ground, metres.
pub const MIN_CLEARANCE: f32 = 1.2;
/// The search lifts the eye at most this far above its preferred height.
const MAX_LIFT: f32 = 10.0;
const LIFT_STEP: f32 = 0.5;
/// How far back the eye may be pulled, as multiples of the preferred
/// distance.
const STRETCHES: [f32; 5] = [1.0, 1.25, 1.6, 2.0, 2.6];
/// Where the eye may stand across the line, metres to the right.
const SIDES: [f32; 6] = [0.8, -0.8, 2.0, -2.0, 3.5, -3.5];
/// Where on the screen (0 top .. 1 bottom) things should sit.
const BALL_LOW: f32 = 0.90;
/// The ball should sit at least this far down (a downhill shot may
/// still raise it, but never above the middle).
const BALL_MID: f32 = 0.55;
const LOOK_AT: f32 = 0.38;
const EDGE: f32 = 0.06;

/// Where the shot looks and from where.
#[derive(Clone, Copy, Debug)]
pub struct Address {
    pub camera: Camera,
    /// The ball could not be seen from any spot the search tried (a ball
    /// in a hollow behind trees): the aim marker is all the player has.
    pub blind: bool,
}

/// One place the eye could be, and how far that is from the preferred one.
struct Candidate {
    stretch: f32,
    lift: f32,
    side: f32,
    cost: f32,
}

fn candidates() -> Vec<Candidate> {
    let mut all = Vec::new();
    for stretch in STRETCHES {
        for lift in 0..=(MAX_LIFT / LIFT_STEP) as usize {
            for side in SIDES {
                let lift = lift as f32 * LIFT_STEP;
                all.push(Candidate {
                    stretch,
                    lift,
                    side,
                    // Height is the dearest move (it flattens the view of
                    // the ball and raises the horizon), then backing off.
                    cost: lift * 1.0 + (stretch - 1.0) * 4.0 + side.abs() * 0.4,
                });
            }
        }
    }
    all.sort_by(|a, b| a.cost.total_cmp(&b.cost));
    all
}

/// The camera for a ball at `ball` (on the ground), shooting along `dir`
/// at a point `distance` metres away. `aspect` is height over width of the
/// picture.
pub fn address(world: &Obstacles, ball: Vec3, dir: Vec2, distance: f32, aspect: f32) -> Address {
    let distance = distance.clamp(15.0, 300.0);
    let far = smoothstep(20.0, 220.0, distance);
    let fov = lerp(58.0, 50.0, far).to_radians();
    let tan_v = (fov * 0.5).tan() * aspect;
    // The angle above the screen's centre of a point at height `f` (0 top,
    // 1 bottom) of the picture.
    let rel = |f: f32| ((0.5 - f) * 2.0 * tan_v).atan();

    let ground = ball.ground();
    let aim = world.on_ground(ground + dir * distance);
    let look = world.on_ground(ground + dir * (0.65 * distance));
    let right = dir.perp();
    let (back0, height0) = (lerp(4.0, 10.0, far), lerp(1.8, 3.6, far));
    let ball_top = Vec3::new(ball.x, ball.y + 0.1, ball.z);

    // For each candidate: the eye, whether it is clear and sees the ball,
    // and the pitch range that keeps the ball and the aim in the picture
    // (empty when none does).
    let try_one = |c: &Candidate| {
        let back = (back0 * c.stretch).min(30.0);
        let xz = ground - dir * back + right * c.side;
        let floor = world.ground(xz.x, xz.y);
        let eye = Vec3::new(xz.x, floor.max(ball.y) + height0 + c.lift, xz.y);
        let up = |p: Vec3| {
            let level = Vec2::new(p.x - eye.x, p.z - eye.z).length().max(0.1);
            (p.y - eye.y).atan2(level)
        };
        // Both between EDGE and 1-EDGE of the picture, the ball no lower
        // than BALL_LOW and, when it can be, in the lower half.
        let low = (up(aim) - rel(EDGE)).max(up(ball) - rel(EDGE));
        let high = (up(ball) - rel(BALL_LOW)).min(up(aim) - rel(1.0 - EDGE));
        // Sideways: the camera turns to the look point, and the ball must
        // stay inside most of the picture's width (an eye shifted far to
        // one side of a close ball would turn it out of the frame).
        let bearing = |p: Vec3| (p.x - eye.x).atan2(-(p.z - eye.z));
        let turned = (bearing(ball) - bearing(look) + std::f32::consts::PI)
            .rem_euclid(std::f32::consts::TAU)
            - std::f32::consts::PI;
        let beside = turned.abs() <= fov * 0.5 * 0.8;
        let fits = beside && low <= high;
        let lower_half = fits && low <= high.min(up(ball) - rel(BALL_MID));
        let above_ground = eye.y >= floor + MIN_CLEARANCE;
        let sees = above_ground && !world.inside_tree(eye) && !world.blocked(eye, ball_top, 1.5);
        let want = (up(look) - rel(LOOK_AT)).min(up(ball) - rel(BALL_MID));
        let pitch = want.clamp(low.min(high), high.max(low));
        (eye, sees, fits, lower_half, above_ground, pitch)
    };

    // The best by preference: seen and framed with the ball in the lower
    // half; then seen and framed; then framed but hidden (the aim marker
    // is what the player has); then seen but badly framed.
    let mut fallback: [Option<(Vec3, f32, bool)>; 3] = [None; 3];
    let mut chosen = None;
    for c in candidates() {
        let (eye, sees, fits, lower, above, pitch) = try_one(&c);
        if !above {
            continue;
        }
        if sees && fits && lower {
            chosen = Some((eye, pitch, true));
            break;
        }
        if sees && fits && fallback[0].is_none() {
            fallback[0] = Some((eye, pitch, true));
        }
        if fits && fallback[1].is_none() {
            fallback[1] = Some((eye, pitch, false));
        }
        if sees && fallback[2].is_none() {
            fallback[2] = Some((eye, pitch, true));
        }
    }
    let (eye, pitch, clear) = chosen
        .or_else(|| fallback.into_iter().flatten().next())
        .unwrap_or_else(|| {
            // Nothing above the ground at all: straight up, looking down.
            let c = &candidates()[0];
            let (eye, _, _, _, _, pitch) = try_one(c);
            (eye, pitch, false)
        });
    let flat = look.ground() - eye.ground();
    let mut camera = Camera::new(eye, yaw_of(flat), pitch);
    camera.fov = fov;
    Address {
        camera,
        blind: !clear,
    }
}
