//! Things that move and are not part of the course: the ball and its
//! shadow. Drawn last, depth-tested against the frame, so a ball goes
//! behind a hill or a tree (docs/golf-gameplay-plan.md, 4.6).

use crate::math::Vec3;

use super::camera::{View, NEAR};
use super::palette as pal;
use super::raster::Frame;

/// A real ball is 4 cm across, a fraction of a pixel at 100 m; it is drawn
/// larger so it can be followed, like every golf game did.
const DRAWN_RADIUS: f32 = 0.09;
const MAX_PIXELS: f32 = 3.0;
/// The ball is not drawn beyond this, metres.
const REACH: f32 = 700.0;

/// The ball: where it is, and the ground below it for its shadow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BallMark {
    pub position: Vec3,
    /// The surface height under the ball.
    pub ground: f32,
}

/// The aim: dots along the ground from the ball, and a ring where the
/// shot is meant to land.
#[derive(Clone, Debug, PartialEq)]
pub struct AimMark {
    pub dots: Vec<Vec3>,
    pub target: Vec3,
    /// The ring's radius on the ground, metres (grows with distance so it
    /// stays about as big on the screen).
    pub radius: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Overlay {
    pub ball: Option<BallMark>,
    pub aim: Option<AimMark>,
}

#[inline]
fn plot(frame: &mut Frame, x: i64, y: i64, iz: f32, index: u8) {
    if x < 0 || y < 0 || x as usize >= frame.width || y as usize >= frame.height {
        return;
    }
    let i = y as usize * frame.width + x as usize;
    if iz > frame.depth[i] {
        frame.index[i] = index;
        frame.depth[i] = iz;
    }
}

/// Draws the overlay into `frame` as seen by `view`.
pub fn draw(frame: &mut Frame, view: &View, overlay: &Overlay) {
    if let Some(aim) = &overlay.aim {
        draw_aim(frame, view, aim);
    }
    let Some(ball) = overlay.ball else {
        return;
    };
    // The shadow first, so the ball covers it when it is on the ground.
    let height = ball.position.y - ball.ground;
    if height > 0.15 {
        let on_ground = Vec3::new(ball.position.x, ball.ground, ball.position.z);
        if let Some((x, y, z)) = screen(view, on_ground) {
            // Flat on the ground: twice as wide as tall.
            let r = (DRAWN_RADIUS * view.focal / z).clamp(1.0, MAX_PIXELS);
            let iz = 1.0 / z;
            for dx in -(2.0 * r).ceil() as i64..=(2.0 * r).ceil() as i64 {
                let wide = r * 2.0;
                if (dx as f32).abs() <= wide {
                    plot(frame, x + dx, y, iz * 1.001, pal::BLACK);
                }
            }
        }
    }
    let Some((x, y, z)) = screen(view, ball.position) else {
        return;
    };
    let r = (DRAWN_RADIUS * view.focal / z).clamp(1.0, MAX_PIXELS);
    // In front of anything at its own depth (the grass it rests on).
    let iz = 1.01 / z;
    let reach = r.ceil() as i64;
    for dy in -reach..=reach {
        for dx in -reach..=reach {
            if (dx * dx + dy * dy) as f32 <= r * r + 0.5 {
                plot(frame, x + dx, y + dy, iz, pal::WHITE);
            }
        }
    }
}

/// Screen pixel and depth of a world point, if it is in front of the eye.
fn screen(view: &View, p: Vec3) -> Option<(i64, i64, f32)> {
    let c = view.to_camera(p);
    if c.z < NEAR * 2.0 || c.z > REACH {
        return None;
    }
    let (sx, sy) = view.project(c);
    Some((sx.floor() as i64, sy.floor() as i64, c.z))
}

/// A touch nearer than the ground it lies on, so the grass does not eat it.
const ON_GROUND: f32 = 1.004;
const RING_POINTS: usize = 48;

fn draw_aim(frame: &mut Frame, view: &View, aim: &AimMark) {
    for &dot in &aim.dots {
        if let Some((x, y, z)) = screen(view, dot) {
            // Two pixels wide: a single one vanishes at a distance.
            plot(frame, x, y, ON_GROUND / z, pal::YELLOW);
            plot(frame, x + 1, y, ON_GROUND / z, pal::YELLOW);
        }
    }
    let t = aim.target;
    for i in 0..RING_POINTS {
        let a = i as f32 / RING_POINTS as f32 * std::f32::consts::TAU;
        let p = Vec3::new(
            t.x + a.cos() * aim.radius,
            t.y + 0.05,
            t.z + a.sin() * aim.radius,
        );
        if let Some((x, y, z)) = screen(view, p) {
            plot(frame, x, y, ON_GROUND / z, pal::YELLOW);
        }
    }
    // A post standing on the ring: the ring alone is a dash seen from a
    // low camera, and the post shows over a rise.
    let top = Vec3::new(t.x, t.y + aim.radius * 2.5, t.z);
    if let (Some((x, y0, z)), Some((_, y1, _))) = (screen(view, t), screen(view, top)) {
        for y in y1.min(y0)..=y1.max(y0) {
            plot(frame, x, y, ON_GROUND / z, pal::YELLOW);
            plot(frame, x + 1, y, ON_GROUND / z, pal::YELLOW);
        }
        for d in -2..=3 {
            plot(frame, x + d, y1, ON_GROUND / z, pal::YELLOW);
        }
    }
    // Cross-hair ticks at the centre.
    for (dx, dz) in [(0.0, 0.0), (0.3, 0.0), (-0.3, 0.0), (0.0, 0.3), (0.0, -0.3)] {
        let p = Vec3::new(t.x + dx * aim.radius, t.y + 0.05, t.z + dz * aim.radius);
        if let Some((x, y, z)) = screen(view, p) {
            plot(frame, x, y, ON_GROUND / z, pal::WHITE);
        }
    }
}
