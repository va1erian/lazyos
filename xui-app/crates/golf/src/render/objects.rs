//! Objects: billboard trees scaled nearest-neighbour from the baked sizes,
//! colour-keyed and depth-tested; posts, markers and flags as small
//! screen-space boxes; the clubhouse and footbridges as flat-shaded meshes.

use crate::course::{Object, ObjectKind};
use crate::math::{Vec2, Vec3};

use super::camera::{View, NEAR};
use super::palette::{self as pal, shade_index};
use super::raster::{triangle, CamVertex, Frame};
use super::sprites::KEY;
use super::terrain::Shading;

/// Draws one object; `time` (seconds) flutters the flags.
pub fn draw(frame: &mut Frame, view: &View, shading: &Shading, object: &Object, time: f32) {
    match object.kind {
        ObjectKind::Tree {
            species,
            variant,
            mirrored,
        } => tree(frame, view, shading, object, species, variant, mirrored),
        ObjectKind::Flagstick { .. } => flag(frame, view, shading, object, time),
        ObjectKind::TeeMarker { set } => {
            let color = match set {
                0 => pal::BLACK,
                1 => pal::WHITE,
                2 => pal::YELLOW,
                3 => pal::RED,
                _ => pal::MARKER_GREEN,
            };
            ball(frame, view, shading, object, color);
        }
        ObjectKind::YardageMarker { metres } => {
            let cap = match metres {
                200 => pal::BLUE,
                150 => pal::WHITE,
                _ => pal::RED,
            };
            post(frame, view, shading, object, 0.15, pal::WHITE, Some(cap));
        }
        ObjectKind::OutOfBoundsStake => post(frame, view, shading, object, 0.08, pal::WHITE, None),
        ObjectKind::Bench => post(frame, view, shading, object, 1.8, pal::WOOD, None),
        ObjectKind::BallWasher => post(frame, view, shading, object, 0.2, pal::BLUE, None),
        ObjectKind::Bridge { angle, length } => bridge(frame, view, shading, object, angle, length),
        ObjectKind::Clubhouse { angle } => clubhouse(frame, view, shading, object, angle),
    }
}

/// The depth (camera z) of an object's base, if it is in front.
pub fn depth(view: &View, object: &Object) -> Option<f32> {
    let c = view.to_camera(Vec3::new(object.position.x, object.base, object.position.y));
    (c.z > NEAR).then_some(c.z)
}

/// Writes `index` at (`x`, `y`) if `iz` is nearer than what is there.
#[inline]
fn plot(frame: &mut Frame, x: usize, y: usize, iz: f32, index: u8) {
    let i = y * frame.width + x;
    if iz > frame.depth[i] {
        frame.index[i] = index;
        frame.depth[i] = iz;
    }
}

#[allow(clippy::too_many_arguments)]
fn tree(
    frame: &mut Frame,
    view: &View,
    shading: &Shading,
    object: &Object,
    species: crate::course::Species,
    variant: u8,
    mirrored: bool,
) {
    let c = view.to_camera(Vec3::new(object.position.x, object.base, object.position.y));
    if c.z < 1.0 || c.z > view.far {
        return;
    }
    let (sx, sy) = view.project(c);
    let tall = object.height * view.focal / c.z;
    // Sprites stand on the ground: test against a depth a little nearer
    // than the base, so the ground right at the trunk does not hide it.
    let iz = 1.02 / c.z;
    if tall < 2.0 {
        let (x, y) = (sx as i64, (sy - 1.0) as i64);
        if x >= 0 && y >= 0 && (x as usize) < frame.width && (y as usize) < frame.height {
            let index = shading.haze(pal::FOLIAGE_A + 4, c.z, x as usize, y as usize);
            plot(frame, x as usize, y as usize, iz, index);
        }
        return;
    }
    let sprite = shading.bake.sprites.get(species, variant, tall);
    let scale = tall / sprite.height as f32;
    let wide = sprite.width as f32 * scale;
    let (x0, y0) = (sx - wide * 0.5, sy - tall);
    if x0 + wide < 0.0 || x0 >= frame.width as f32 || sy < 0.0 || y0 >= frame.height as f32 {
        return;
    }
    let xs = ((x0 - 0.5).ceil().max(0.0) as usize)
        ..(((x0 + wide - 0.5).ceil()).min(frame.width as f32).max(0.0) as usize);
    let ys = ((y0 - 0.5).ceil().max(0.0) as usize)
        ..(((sy - 0.5).ceil()).min(frame.height as f32).max(0.0) as usize);
    for x in xs {
        let mut column = ((x as f32 + 0.5 - x0) / scale) as usize;
        column = column.min(sprite.width - 1);
        if mirrored {
            column = sprite.width - 1 - column;
        }
        for y in ys.clone() {
            let row = (((y as f32 + 0.5 - y0) / scale) as usize).min(sprite.height - 1);
            let texel = sprite.pixels[row * sprite.width + column];
            if texel == KEY {
                continue;
            }
            let i = y * frame.width + x;
            if iz > frame.depth[i] {
                frame.index[i] = shading.haze(texel, c.z, x, y);
                frame.depth[i] = iz;
            }
        }
    }
}

/// A screen-space box `width` metres wide standing on the object's base;
/// `cap` colours its top fifth.
fn post(
    frame: &mut Frame,
    view: &View,
    shading: &Shading,
    object: &Object,
    width: f32,
    color: u8,
    cap: Option<u8>,
) {
    let c = view.to_camera(Vec3::new(object.position.x, object.base, object.position.y));
    if c.z < NEAR * 2.0 || c.z > view.far * 0.5 {
        return;
    }
    let (sx, sy) = view.project(c);
    let k = view.focal / c.z;
    let (w, h) = ((width * k).max(1.0), (object.height * k).max(1.0));
    let iz = 1.01 / c.z;
    let top = sy - h;
    for y in (top.max(0.0) as i64)..(sy.min(frame.height as f32) as i64) {
        let index = match cap {
            Some(cap) if (y as f32) < top + h * 0.2 => cap,
            _ => color,
        };
        let hazed = shading.haze(index, c.z, 0, y as usize);
        for x in ((sx - w * 0.5).max(0.0) as i64)..((sx + w * 0.5).min(frame.width as f32) as i64) {
            plot(frame, x as usize, y as usize, iz, hazed);
        }
    }
}

/// A tee marker: a ball sitting on the ground.
fn ball(frame: &mut Frame, view: &View, shading: &Shading, object: &Object, color: u8) {
    let radius = object.height * 0.5;
    let c = view.to_camera(Vec3::new(
        object.position.x,
        object.base + radius,
        object.position.y,
    ));
    if c.z < NEAR * 2.0 || c.z > view.far * 0.4 {
        return;
    }
    let (sx, sy) = view.project(c);
    let r = (radius * view.focal / c.z).max(0.7);
    let iz = 1.01 / c.z;
    let hazed = shading.haze(color, c.z, 0, 0);
    let reach = r.ceil() as i64;
    for dy in -reach..=reach {
        for dx in -reach..=reach {
            let (x, y) = (sx as i64 + dx, sy as i64 + dy);
            let inside = (dx * dx + dy * dy) as f32 <= r * r + 0.5;
            if inside
                && x >= 0
                && y >= 0
                && (x as usize) < frame.width
                && (y as usize) < frame.height
            {
                plot(frame, x as usize, y as usize, iz, hazed);
            }
        }
    }
}

/// The flagstick: a white pole and a red flag that ripples in the wind.
fn flag(frame: &mut Frame, view: &View, shading: &Shading, object: &Object, time: f32) {
    post(frame, view, shading, object, 0.035, pal::WHITE, None);
    let top = Vec3::new(
        object.position.x,
        object.base + object.height,
        object.position.y,
    );
    let c = view.to_camera(top);
    if c.z < NEAR * 2.0 || c.z > view.far * 0.5 {
        return;
    }
    let (sx, sy) = view.project(c);
    let k = view.focal / c.z;
    let (w, h) = ((0.55 * k).max(2.0), (0.36 * k).max(1.0));
    let iz = 1.01 / c.z;
    let red = shading.haze(pal::RED, c.z, 0, 0);
    let columns = w as i64;
    for dx in 0..columns {
        let wave = ((time * 7.0 - dx as f32 / w * 5.0).sin() * h * 0.12 * dx as f32 / w) as i64;
        let x = sx as i64 + dx;
        if x < 0 || x >= frame.width as i64 {
            continue;
        }
        for dy in 0..h as i64 {
            let y = sy as i64 + dy + wave;
            if y >= 0 && y < frame.height as i64 {
                plot(frame, x as usize, y as usize, iz, red);
            }
        }
    }
}

/// A flat-shaded triangle in world space, `ramp` lit by the sun.
fn face(
    frame: &mut Frame,
    view: &View,
    shading: &Shading,
    tri: [Vec3; 3],
    ramp: u8,
    two_sided: bool,
) {
    let normal = (tri[1] - tri[0]).cross(tri[2] - tri[0]).normalized();
    let facing = normal.dot(view.eye - tri[0]);
    if facing <= 0.0 && !two_sided {
        return;
    }
    let lit = if facing > 0.0 { normal } else { normal * -1.0 };
    let light = ((0.4 + 0.6 * lit.dot(shading.bake.sun).max(0.0)) * 220.0) as i32;
    let cam = tri.map(|p| CamVertex {
        c: view.to_camera(p),
        u: p.x,
        v: p.z,
    });
    triangle(frame, view, cam, &mut |x, y, z, _, _| {
        shading.haze(shade_index(ramp, light, x, y), z, x, y)
    });
}

/// A quad `a b c d` (counter-clockwise seen from outside).
fn quad(
    frame: &mut Frame,
    view: &View,
    shading: &Shading,
    q: [Vec3; 4],
    ramp: u8,
    two_sided: bool,
) {
    face(frame, view, shading, [q[0], q[1], q[2]], ramp, two_sided);
    face(frame, view, shading, [q[0], q[2], q[3]], ramp, two_sided);
}

/// The clubhouse: walls under a gabled roof, 24 x 14 m.
fn clubhouse(frame: &mut Frame, view: &View, shading: &Shading, object: &Object, angle: f32) {
    let along = Vec2::from_angle(angle);
    let across = along.perp();
    let (hl, hw, wall, ridge) = (12.0, 7.0, 4.5, object.height);
    let at = |a: f32, b: f32, y: f32| {
        let p = object.position + along * a + across * b;
        Vec3::new(p.x, object.base + y, p.y)
    };
    let corners = [(-hl, -hw), (hl, -hw), (hl, hw), (-hl, hw)];
    for k in 0..4 {
        let (a, b) = (corners[k], corners[(k + 1) % 4]);
        // Walls wind so their normals face outward.
        quad(
            frame,
            view,
            shading,
            [
                at(a.0, a.1, -1.0),
                at(a.0, a.1, wall),
                at(b.0, b.1, wall),
                at(b.0, b.1, -1.0),
            ],
            pal::WALL,
            true,
        );
    }
    for side in [-1.0f32, 1.0] {
        let roof = [
            at(-hl - 0.6, side * (hw + 0.6), wall - 0.3),
            at(-hl - 0.6, 0.0, ridge),
            at(hl + 0.6, 0.0, ridge),
            at(hl + 0.6, side * (hw + 0.6), wall - 0.3),
        ];
        quad(frame, view, shading, roof, pal::ROOF, true);
        let gable = [
            at(side * hl, -hw, wall),
            at(side * hl, 0.0, ridge),
            at(side * hl, hw, wall),
        ];
        face(frame, view, shading, gable, pal::WALL, true);
    }
}

/// A footbridge: a plank deck between the banks.
fn bridge(
    frame: &mut Frame,
    view: &View,
    shading: &Shading,
    object: &Object,
    angle: f32,
    length: f32,
) {
    let along = Vec2::from_angle(angle) * (length * 0.5);
    let across = Vec2::from_angle(angle).perp() * 1.4;
    let at = |p: Vec2, y: f32| Vec3::new(p.x, object.base + y, p.y);
    let c = object.position;
    let deck = [
        at(c - along - across, 0.0),
        at(c + along - across, 0.0),
        at(c + along + across, 0.0),
        at(c - along + across, 0.0),
    ];
    quad(frame, view, shading, deck, pal::STONE, true);
    for side in [-1.0, 1.0] {
        let rail = across * side;
        quad(
            frame,
            view,
            shading,
            [
                at(c - along + rail, 0.0),
                at(c + along + rail, 0.0),
                at(c + along + rail, 0.9),
                at(c - along + rail, 0.9),
            ],
            pal::STONE,
            true,
        );
    }
}
