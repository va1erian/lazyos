//! The overhead map: the course top-down from its materials, hill-shaded,
//! with the holes' greens, tees and numbers marked. Built once per course
//! (milestone 1 of the design: it validates the generator at a glance).

use crate::course::{Course, Material, ObjectKind};
use crate::math::Vec2;

/// A top-down colour for each material.
pub fn color(m: Material) -> [u8; 3] {
    match m {
        Material::Green => [86, 196, 92],
        Material::Fringe => [74, 170, 70],
        Material::TeeBox => [92, 186, 84],
        Material::Fairway => [96, 170, 60],
        Material::FirstCut => [78, 146, 52],
        Material::Rough => [62, 122, 44],
        Material::DeepRough => [116, 128, 58],
        Material::Sand => [226, 210, 150],
        Material::WasteArea => [196, 176, 118],
        Material::Water => [52, 104, 168],
        Material::Woodland => [34, 82, 36],
        Material::CartPath => [168, 164, 150],
        Material::Rock => [128, 124, 116],
        Material::OutOfBounds => [70, 86, 50],
    }
}

/// The map: `scale` metres per pixel, RGBA8, north up.
pub struct Minimap {
    pub width: usize,
    pub height: usize,
    pub scale: f32,
    pub rgba: Vec<u8>,
}

impl Minimap {
    pub fn new(course: &Course, pixels: usize) -> Minimap {
        let size = course.width as usize;
        let scale = size as f32 / pixels as f32;
        let mut rgba = vec![0u8; pixels * pixels * 4];
        let light = Vec2::new(-0.6, -0.8);
        for py in 0..pixels {
            for px in 0..pixels {
                let (x, z) = ((px as f32 + 0.5) * scale, (py as f32 + 0.5) * scale);
                let m = course.material_at(x, z);
                let dx =
                    course.elevation.sample(x + scale, z) - course.elevation.sample(x - scale, z);
                let dz =
                    course.elevation.sample(x, z + scale) - course.elevation.sample(x, z - scale);
                let slope = Vec2::new(dx, dz) * (1.0 / (2.0 * scale));
                let shade = if m == Material::Water {
                    1.0
                } else {
                    (1.0 - 2.2 * slope.dot(light)).clamp(0.55, 1.35)
                };
                let [r, g, b] = color(m);
                let k = &mut rgba[(py * pixels + px) * 4..][..4];
                k[0] = (r as f32 * shade).min(255.0) as u8;
                k[1] = (g as f32 * shade).min(255.0) as u8;
                k[2] = (b as f32 * shade).min(255.0) as u8;
                k[3] = 255;
            }
        }
        let mut map = Minimap {
            width: pixels,
            height: pixels,
            scale,
            rgba,
        };
        for object in &course.objects {
            if let ObjectKind::Tree { .. } = object.kind {
                map.dot(object.position, 0.0, [24, 58, 26]);
            }
        }
        for object in &course.objects {
            match object.kind {
                ObjectKind::Flagstick { .. } => map.dot(object.position, 1.0, [230, 40, 40]),
                ObjectKind::Clubhouse { .. } => map.dot(object.position, 3.0, [150, 60, 40]),
                _ => {}
            }
        }
        map
    }

    /// The pixel under world point `p`.
    pub fn to_pixel(&self, p: Vec2) -> (f32, f32) {
        (p.x / self.scale, p.y / self.scale)
    }

    /// A filled square of `radius` pixels.
    fn dot(&mut self, p: Vec2, radius: f32, color: [u8; 3]) {
        let (cx, cy) = self.to_pixel(p);
        let r = radius.ceil() as i64;
        for dy in -r..=r {
            for dx in -r..=r {
                let (x, y) = (cx as i64 + dx, cy as i64 + dy);
                if x < 0 || y < 0 || x >= self.width as i64 || y >= self.height as i64 {
                    continue;
                }
                let k = &mut self.rgba[(y as usize * self.width + x as usize) * 4..][..3];
                k.copy_from_slice(&color);
            }
        }
    }
}
