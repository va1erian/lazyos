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

/// One arrow of the green's slope grid: a short line on the ground.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridArrow {
    pub from: Vec3,
    pub to: Vec3,
    /// Palette index.
    pub color: u8,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Overlay {
    pub ball: Option<BallMark>,
    pub aim: Option<AimMark>,
    pub grid: Vec<GridArrow>,
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
    for arrow in &overlay.grid {
        draw_arrow(frame, view, arrow);
    }
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
            let iz = grazing(view, ball.ground, z) / z;
            for dx in -(2.0 * r).ceil() as i64..=(2.0 * r).ceil() as i64 {
                let wide = r * 2.0;
                if (dx as f32).abs() <= wide {
                    plot(frame, x + dx, y, iz, pal::BLACK);
                }
            }
        }
    }
    let Some((x, y, z)) = screen(view, ball.position) else {
        return;
    };
    let r = (DRAWN_RADIUS * view.focal / z).clamp(1.0, MAX_PIXELS);
    // In front of anything at its own depth (the grass it rests on).
    let iz = grazing(view, ball.ground, z) / z;
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

/// How much nearer than the surface under it a thing standing on the
/// ground is tested: seen from low and far, one row of pixels spans
/// metres of depth, and a fixed margin would hide the thing.
fn grazing(view: &View, ground: f32, z: f32) -> f32 {
    let height = (view.eye.y - ground).max(1.0);
    1.0 + (2.5 * z / (height * view.focal)).clamp(0.01, 0.4)
}
const RING_POINTS: usize = 48;

fn draw_aim(frame: &mut Frame, view: &View, aim: &AimMark) {
    for &dot in &aim.dots {
        if let Some((x, y, z)) = screen(view, dot) {
            // Two pixels wide: a single one vanishes at a distance.
            let iz = grazing(view, dot.y, z).max(ON_GROUND) / z;
            plot(frame, x, y, iz, pal::YELLOW);
            plot(frame, x + 1, y, iz, pal::YELLOW);
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
    if let (Some((x, y0, z0)), Some((_, y1, z1))) = (screen(view, t), screen(view, top)) {
        // Depth is tested where each pixel of the post is, not at its
        // foot: from a high camera the top is nearer than the base, and a
        // rise between the two would otherwise hide the part that shows.
        // 1/z is linear along a line on the screen.
        let (iz0, iz1) = (1.0 / z0, 1.0 / z1);
        for y in y1.min(y0)..=y1.max(y0) {
            let k = if y1 == y0 {
                0.0
            } else {
                (y - y0) as f32 / (y1 - y0) as f32
            };
            let iz = (iz0 + (iz1 - iz0) * k) * ON_GROUND;
            plot(frame, x, y, iz, pal::YELLOW);
            plot(frame, x + 1, y, iz, pal::YELLOW);
        }
        for d in -2..=3 {
            plot(frame, x + d, y1, iz1 * ON_GROUND, pal::YELLOW);
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

/// A grid arrow: the shaft as a line of pixels from tail to head, and the
/// head a pixel bigger.
fn draw_arrow(frame: &mut Frame, view: &View, arrow: &GridArrow) {
    let (Some((x0, y0, z0)), Some((x1, y1, _))) =
        (screen(view, arrow.from), screen(view, arrow.to))
    else {
        return;
    };
    if z0 > 70.0 {
        return;
    }
    let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
    let iz = ON_GROUND / z0;
    for i in 0..=steps {
        let x = x0 + (x1 - x0) * i / steps;
        let y = y0 + (y1 - y0) * i / steps;
        plot(frame, x, y, iz, arrow.color);
    }
    plot(frame, x1 + 1, y1, iz, arrow.color);
    plot(frame, x1, y1 - 1, iz, arrow.color);
}
