//! The camera: an eye with a yaw and a pitch, a horizontal field of view
//! and the perspective projection onto the framebuffer.

use crate::math::Vec3;

/// Things nearer than this are clipped.
pub const NEAR: f32 = 0.25;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub eye: Vec3,
    /// Radians clockwise from north (-z) seen from above: 0 looks north,
    /// pi/2 looks east.
    pub yaw: f32,
    /// Radians above the horizon.
    pub pitch: f32,
    /// Horizontal field of view, radians.
    pub fov: f32,
    /// Beyond this the backdrop takes over, metres.
    pub far: f32,
}

/// The camera prepared for one framebuffer size.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub eye: Vec3,
    pub right: Vec3,
    pub up: Vec3,
    pub forward: Vec3,
    pub focal: f32,
    pub cx: f32,
    pub cy: f32,
    pub width: usize,
    pub height: usize,
    pub far: f32,
    pub yaw: f32,
    pub pitch: f32,
}

impl Camera {
    pub fn new(eye: Vec3, yaw: f32, pitch: f32) -> Camera {
        Camera {
            eye,
            yaw,
            pitch,
            fov: 60f32.to_radians(),
            far: 900.0,
        }
    }

    /// The unit vector the camera looks along.
    pub fn forward(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(sy * cp, sp, -cy * cp)
    }

    pub fn view(&self, width: usize, height: usize) -> View {
        let forward = self.forward();
        let right = forward.cross(Vec3::new(0.0, 1.0, 0.0)).normalized();
        let up = right.cross(forward);
        View {
            eye: self.eye,
            right,
            up,
            forward,
            focal: width as f32 * 0.5 / (self.fov * 0.5).tan(),
            cx: width as f32 * 0.5,
            cy: height as f32 * 0.5,
            width,
            height,
            far: self.far,
            yaw: self.yaw,
            pitch: self.pitch,
        }
    }
}

impl View {
    /// World point to camera space: (right, up, depth).
    #[inline]
    pub fn to_camera(&self, p: Vec3) -> Vec3 {
        let d = p - self.eye;
        Vec3::new(d.dot(self.right), d.dot(self.up), d.dot(self.forward))
    }

    /// Camera space to screen pixels (x right, y down); `z` must be > 0.
    #[inline]
    pub fn project(&self, c: Vec3) -> (f32, f32) {
        let k = self.focal / c.z;
        (self.cx + c.x * k, self.cy - c.y * k)
    }

    /// Whether an axis-aligned box may be in view: tested against the
    /// near and far planes and the four sides of the frustum.
    pub fn sees_box(&self, lo: Vec3, hi: Vec3) -> bool {
        let half_w = self.cx / self.focal;
        let half_h = self.cy / self.focal;
        // Inward plane normals in camera space (through the eye).
        let planes = [
            Vec3::new(1.0, 0.0, half_w),
            Vec3::new(-1.0, 0.0, half_w),
            Vec3::new(0.0, 1.0, half_h),
            Vec3::new(0.0, -1.0, half_h),
        ];
        let corners = [
            Vec3::new(lo.x, lo.y, lo.z),
            Vec3::new(hi.x, lo.y, lo.z),
            Vec3::new(lo.x, hi.y, lo.z),
            Vec3::new(hi.x, hi.y, lo.z),
            Vec3::new(lo.x, lo.y, hi.z),
            Vec3::new(hi.x, lo.y, hi.z),
            Vec3::new(lo.x, hi.y, hi.z),
            Vec3::new(hi.x, hi.y, hi.z),
        ];
        let cam = corners.map(|p| self.to_camera(p));
        if cam.iter().all(|c| c.z < NEAR) || cam.iter().all(|c| c.z > self.far) {
            return false;
        }
        planes.iter().all(|n| cam.iter().any(|c| c.dot(*n) >= 0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looking_north_right_is_east() {
        let cam = Camera::new(Vec3::new(0.0, 0.0, 0.0), 0.0, 0.0);
        let v = cam.view(640, 360);
        let c = v.to_camera(Vec3::new(10.0, 0.0, -100.0));
        assert!(c.x > 0.0 && c.z > 0.0);
        let (sx, sy) = v.project(c);
        assert!(sx > 320.0 && (sy - 180.0).abs() < 1e-3);
        let up = v.project(v.to_camera(Vec3::new(0.0, 10.0, -100.0)));
        assert!(up.1 < 180.0);
    }

    #[test]
    fn frustum_culls_behind_and_beside() {
        let cam = Camera::new(Vec3::new(0.0, 2.0, 0.0), 0.0, 0.0);
        let v = cam.view(640, 360);
        assert!(v.sees_box(Vec3::new(-5.0, 0.0, -60.0), Vec3::new(5.0, 3.0, -50.0)));
        assert!(!v.sees_box(Vec3::new(-5.0, 0.0, 50.0), Vec3::new(5.0, 3.0, 60.0)));
        assert!(!v.sees_box(Vec3::new(200.0, 0.0, -60.0), Vec3::new(210.0, 3.0, -50.0)));
        assert!(!v.sees_box(Vec3::new(-5.0, 0.0, -2000.0), Vec3::new(5.0, 3.0, -1990.0)));
    }
}
