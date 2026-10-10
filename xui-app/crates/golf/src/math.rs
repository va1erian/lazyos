//! Small vector types and the scalar helpers the generator and the renderer
//! share. World space: `x` east, `z` south (rows of the heightfield), `y` up,
//! all in metres.

use std::ops::{Add, Mul, Sub};

/// A point or direction on the ground plane (`x`, `z`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

impl Vec2 {
    pub const fn new(x: f32, y: f32) -> Vec2 {
        Vec2 { x, y }
    }

    /// The unit vector at `angle` radians from +x, turning toward +y.
    pub fn from_angle(angle: f32) -> Vec2 {
        Vec2::new(angle.cos(), angle.sin())
    }

    pub fn dot(self, other: Vec2) -> f32 {
        self.x * other.x + self.y * other.y
    }

    /// The z component of the 3D cross product: positive when `other` turns
    /// counter-clockwise from `self`.
    pub fn cross(self, other: Vec2) -> f32 {
        self.x * other.y - self.y * other.x
    }

    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    pub fn distance(self, other: Vec2) -> f32 {
        (self - other).length()
    }

    /// The unit vector along `self`, or +x for a zero vector.
    pub fn normalized(self) -> Vec2 {
        let length = self.length();
        if length > 1e-6 {
            self * (1.0 / length)
        } else {
            Vec2::new(1.0, 0.0)
        }
    }

    /// `self` turned a quarter counter-clockwise.
    pub fn perp(self) -> Vec2 {
        Vec2::new(-self.y, self.x)
    }

    pub fn angle(self) -> f32 {
        self.y.atan2(self.x)
    }

    pub fn lerp(self, other: Vec2, t: f32) -> Vec2 {
        self + (other - self) * t
    }
}

impl Add for Vec2 {
    type Output = Vec2;
    fn add(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x + o.x, self.y + o.y)
    }
}

impl Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x - o.x, self.y - o.y)
    }
}

impl Mul<f32> for Vec2 {
    type Output = Vec2;
    fn mul(self, s: f32) -> Vec2 {
        Vec2::new(self.x * s, self.y * s)
    }
}

/// A point or direction in world space.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const fn new(x: f32, y: f32, z: f32) -> Vec3 {
        Vec3 { x, y, z }
    }

    pub fn dot(self, o: Vec3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    pub fn cross(self, o: Vec3) -> Vec3 {
        Vec3::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }

    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }

    pub fn normalized(self) -> Vec3 {
        let length = self.length();
        if length > 1e-6 {
            self * (1.0 / length)
        } else {
            Vec3::new(0.0, 1.0, 0.0)
        }
    }

    /// The ground-plane part (`x`, `z`).
    pub fn ground(self) -> Vec2 {
        Vec2::new(self.x, self.z)
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    fn add(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}

impl Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}

impl Mul<f32> for Vec3 {
    type Output = Vec3;
    fn mul(self, s: f32) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Hermite step from 0 at `edge0` to 1 at `edge1` (either order).
pub fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Distance from `p` to the segment `a`-`b`, and where along it (0..=1) the
/// nearest point lies.
pub fn segment_distance(p: Vec2, a: Vec2, b: Vec2) -> (f32, f32) {
    let ab = b - a;
    let len2 = ab.dot(ab);
    let t = if len2 > 1e-9 {
        ((p - a).dot(ab) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    ((a + ab * t).distance(p), t)
}

/// Distance from `p` to a polyline, with the arc length at the nearest point.
pub fn polyline_distance(p: Vec2, line: &[Vec2]) -> (f32, f32) {
    let mut best = (f32::MAX, 0.0);
    let mut walked = 0.0;
    for pair in line.windows(2) {
        let (d, t) = segment_distance(p, pair[0], pair[1]);
        let len = pair[0].distance(pair[1]);
        if d < best.0 {
            best = (d, walked + t * len);
        }
        walked += len;
    }
    best
}

/// Signed distance from `p` to a polyline (positive on the side its
/// direction's `perp` points to), with the arc length at the nearest point.
pub fn polyline_signed(p: Vec2, line: &[Vec2]) -> (f32, f32) {
    let mut best = (f32::MAX, 0.0, 1.0);
    let mut walked = 0.0;
    for pair in line.windows(2) {
        let (d, t) = segment_distance(p, pair[0], pair[1]);
        let len = pair[0].distance(pair[1]);
        if d < best.0 {
            let side = (pair[1] - pair[0]).cross(p - pair[0]);
            best = (d, walked + t * len, if side < 0.0 { -1.0 } else { 1.0 });
        }
        walked += len;
    }
    (best.0 * best.2, best.1)
}

/// The polyline's total length.
pub fn polyline_length(line: &[Vec2]) -> f32 {
    line.windows(2).map(|p| p[0].distance(p[1])).sum()
}

/// The point `distance` metres along a polyline (clamped to its ends) and
/// the direction of travel there.
pub fn polyline_at(line: &[Vec2], distance: f32) -> (Vec2, Vec2) {
    let mut left = distance.max(0.0);
    for pair in line.windows(2) {
        let len = pair[0].distance(pair[1]);
        if left <= len || len <= 0.0 {
            let dir = (pair[1] - pair[0]).normalized();
            return (pair[0] + dir * left.min(len), dir);
        }
        left -= len;
    }
    match line {
        [.., a, b] => (*b, (*b - *a).normalized()),
        [a] => (*a, Vec2::new(1.0, 0.0)),
        [] => (Vec2::default(), Vec2::new(1.0, 0.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polyline_walks() {
        let line = [
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(10.0, 10.0),
        ];
        assert_eq!(polyline_length(&line), 20.0);
        let (p, d) = polyline_at(&line, 15.0);
        assert!((p.x - 10.0).abs() < 1e-5 && (p.y - 5.0).abs() < 1e-5);
        assert!((d.y - 1.0).abs() < 1e-5);
        let (dist, along) = polyline_distance(Vec2::new(12.0, 5.0), &line);
        assert!((dist - 2.0).abs() < 1e-5 && (along - 15.0).abs() < 1e-5);
    }

    #[test]
    fn smoothstep_clamps() {
        assert_eq!(smoothstep(0.0, 1.0, -1.0), 0.0);
        assert_eq!(smoothstep(0.0, 1.0, 2.0), 1.0);
        assert_eq!(smoothstep(1.0, 0.0, 0.0), 1.0);
    }
}
