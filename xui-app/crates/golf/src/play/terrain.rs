//! What the ball needs to know about the ground: its height, its material,
//! its slope, where the trees are and where the hole is. A trait, so the
//! shot physics is tested on flat and tilted planes as easily as on a
//! generated course.

use crate::course::{Course, Material};
use crate::math::{Vec2, Vec3};
use crate::render::Bake;

use super::cam::Obstacles;

pub trait Terrain {
    /// The surface height (the water level on water).
    fn height(&self, x: f32, z: f32) -> f32;

    fn material(&self, x: f32, z: f32) -> Material;

    /// Whether `p` is inside a tree.
    fn solid(&self, _p: Vec3) -> bool {
        false
    }

    /// The hole being played.
    fn pin(&self) -> Option<Vec2> {
        None
    }

    /// The upward unit normal, from central differences half a metre
    /// either side.
    fn normal(&self, x: f32, z: f32) -> Vec3 {
        const H: f32 = 0.5;
        let dx = self.height(x + H, z) - self.height(x - H, z);
        let dz = self.height(x, z + H) - self.height(x, z - H);
        Vec3::new(-dx / (2.0 * H), 1.0, -dz / (2.0 * H)).normalized()
    }
}

/// A generated course, for one hole's pin.
pub struct CourseTerrain<'a> {
    pub course: &'a Course,
    pub bake: &'a Bake,
    pub pin: Vec2,
}

impl<'a> CourseTerrain<'a> {
    pub fn new(course: &'a Course, bake: &'a Bake, hole: usize) -> CourseTerrain<'a> {
        let pin = course.holes.get(hole).map_or(Vec2::default(), |h| h.pin);
        CourseTerrain { course, bake, pin }
    }

    fn inside(&self, x: f32, z: f32) -> bool {
        let size = self.bake.size as f32;
        x >= 0.0 && z >= 0.0 && x < size && z < size
    }
}

impl Terrain for CourseTerrain<'_> {
    fn height(&self, x: f32, z: f32) -> f32 {
        self.bake.height_at(x, z)
    }

    fn material(&self, x: f32, z: f32) -> Material {
        if self.inside(x, z) {
            self.course.material_at(x, z)
        } else {
            Material::OutOfBounds
        }
    }

    fn solid(&self, p: Vec3) -> bool {
        Obstacles { bake: self.bake }.inside_tree(p)
    }

    fn pin(&self) -> Option<Vec2> {
        Some(self.pin)
    }
}

/// A plane for tests: height `slope` (x, z) per metre, one material.
pub struct Plane {
    pub slope: Vec2,
    pub material: Material,
    pub pin: Option<Vec2>,
}

impl Terrain for Plane {
    fn height(&self, x: f32, z: f32) -> f32 {
        x * self.slope.x + z * self.slope.y
    }

    fn material(&self, _x: f32, _z: f32) -> Material {
        self.material
    }

    fn pin(&self) -> Option<Vec2> {
        self.pin
    }
}

/// A terrain seen without its pin, and optionally without its trees: what
/// the caddy plays on, so a putt that would drop is measured by where it
/// would have gone, and a drive's reach is not cut short by a tree.
pub struct Filtered<'a> {
    pub inner: &'a dyn Terrain,
    /// Whether trees are solid.
    pub trees: bool,
}

impl Terrain for Filtered<'_> {
    fn height(&self, x: f32, z: f32) -> f32 {
        self.inner.height(x, z)
    }

    fn material(&self, x: f32, z: f32) -> Material {
        self.inner.material(x, z)
    }

    fn solid(&self, p: Vec3) -> bool {
        self.trees && self.inner.solid(p)
    }
}
