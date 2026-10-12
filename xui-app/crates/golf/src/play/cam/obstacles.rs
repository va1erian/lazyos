//! What can stand between the camera and the ball: the ground and the
//! trees. Trees are billboards in the renderer; here each is a trunk
//! (a thin cylinder) under a canopy (a fat one), which is what the eye
//! reads as "something in the way".

use crate::course::{Object, ObjectKind};
use crate::math::{Vec2, Vec3};
use crate::render::Bake;

const TRUNK_RADIUS: f32 = 0.4;
/// A canopy starts this far up the tree (as a fraction of its height).
const CANOPY_BASE: f32 = 0.3;

/// The solid parts of the course, as the camera sees them.
pub struct Obstacles<'a> {
    pub bake: &'a Bake,
}

/// One tree as two cylinders.
struct Tree {
    at: Vec2,
    base: f32,
    top: f32,
    canopy_radius: f32,
}

impl Tree {
    fn of(object: &Object) -> Option<Tree> {
        matches!(object.kind, ObjectKind::Tree { .. }).then(|| Tree {
            at: object.position,
            base: object.base,
            top: object.base + object.height,
            canopy_radius: (object.height * 0.3).clamp(0.8, 5.0),
        })
    }

    /// Whether `p` is inside the trunk or the canopy.
    fn contains(&self, p: Vec3) -> bool {
        if p.y < self.base || p.y > self.top {
            return false;
        }
        let d = p.ground().distance(self.at);
        let canopy_low = self.base + (self.top - self.base) * CANOPY_BASE;
        d <= TRUNK_RADIUS || (p.y >= canopy_low && d <= self.canopy_radius)
    }
}

impl Obstacles<'_> {
    pub fn ground(&self, x: f32, z: f32) -> f32 {
        self.bake.height_at(x, z)
    }

    /// The ground point under (`x`, `z`) as a world point.
    pub fn on_ground(&self, p: Vec2) -> Vec3 {
        Vec3::new(p.x, self.ground(p.x, p.y), p.y)
    }

    fn trees_near(&self, p: Vec2, radius: f32) -> Vec<Tree> {
        self.bake
            .objects_near(p, radius)
            .filter_map(Tree::of)
            .collect()
    }

    /// Whether `p` is inside a tree.
    pub fn inside_tree(&self, p: Vec3) -> bool {
        self.trees_near(p.ground(), 6.0)
            .iter()
            .any(|t| t.contains(p))
    }

    /// Whether the straight line from `from` to `to` meets the ground or a
    /// tree, ignoring the last `slack` metres before `to` (the ball sits
    /// on the ground, and the slope right behind it may rise a little).
    pub fn blocked(&self, from: Vec3, to: Vec3, slack: f32) -> bool {
        let line = to - from;
        let length = line.length();
        if length < 1e-3 {
            return false;
        }
        let steps = (length / 0.75).ceil().max(2.0) as usize;
        let mid = (from + line * 0.5).ground();
        let trees = self.trees_near(mid, length * 0.5 + 6.0);
        for i in 1..steps {
            let along = i as f32 / steps as f32;
            if (1.0 - along) * length < slack {
                break;
            }
            let p = from + line * along;
            if p.y < self.ground(p.x, p.z) + 0.1 || trees.iter().any(|t| t.contains(p)) {
                return true;
            }
        }
        false
    }
}
