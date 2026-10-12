//! The little linear algebra the renderer needs: a 3-vector and a rotation.

use std::ops::{Add, Mul, Neg, Sub};

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

    /// Unit length, or the vector itself when it has none.
    pub fn normalized(self) -> Vec3 {
        let l = self.length();
        if l > 1e-12 {
            self * (1.0 / l)
        } else {
            self
        }
    }

    /// `self` mirrored about the unit normal `n`.
    pub fn reflect(self, n: Vec3) -> Vec3 {
        self - n * (2.0 * self.dot(n))
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

impl Neg for Vec3 {
    type Output = Vec3;
    fn neg(self) -> Vec3 {
        Vec3::new(-self.x, -self.y, -self.z)
    }
}

/// A rotation as three rows; `apply` is `M * v`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat3(pub [Vec3; 3]);

impl Mat3 {
    pub const IDENTITY: Mat3 = Mat3([
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        Vec3::new(0.0, 0.0, 1.0),
    ]);

    /// A turn about the Y axis (left/right).
    pub fn yaw(a: f32) -> Mat3 {
        let (s, c) = a.sin_cos();
        Mat3([
            Vec3::new(c, 0.0, s),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(-s, 0.0, c),
        ])
    }

    /// A turn about the X axis (up/down).
    pub fn pitch(a: f32) -> Mat3 {
        let (s, c) = a.sin_cos();
        Mat3([
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(0.0, c, -s),
            Vec3::new(0.0, s, c),
        ])
    }

    /// A turn about the Z axis (roll).
    pub fn roll(a: f32) -> Mat3 {
        let (s, c) = a.sin_cos();
        Mat3([
            Vec3::new(c, -s, 0.0),
            Vec3::new(s, c, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
        ])
    }

    pub fn apply(&self, v: Vec3) -> Vec3 {
        Vec3::new(self.0[0].dot(v), self.0[1].dot(v), self.0[2].dot(v))
    }

    /// `self` applied after `other`: `self * other`.
    pub fn then_after(&self, other: &Mat3) -> Mat3 {
        let col = |j: usize| Vec3::new(other.0[0].comp(j), other.0[1].comp(j), other.0[2].comp(j));
        let row = |r: Vec3| Vec3::new(r.dot(col(0)), r.dot(col(1)), r.dot(col(2)));
        Mat3([row(self.0[0]), row(self.0[1]), row(self.0[2])])
    }
}

impl Vec3 {
    fn comp(self, i: usize) -> f32 {
        [self.x, self.y, self.z][i]
    }
}
