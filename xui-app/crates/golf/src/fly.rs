//! Flying over the course: which keys move the camera, and the camera's
//! motion from the held keys, the mouse and the wheel.

use xui_core::message::Key;

use crate::course::Hole;
use crate::math::{Vec2, Vec3};
use crate::render::Camera;

/// A movement held down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Forward,
    Back,
    Left,
    Right,
    Up,
    Down,
    TurnLeft,
    TurnRight,
    Fast,
}

impl Action {
    fn bit(self) -> u16 {
        1 << self as u16
    }
}

/// The movement a key holds, if any: WASD (or arrows) to move, Space/E
/// and C/Q (or Page Up/Down) to climb and sink, Shift to go fast.
pub fn action_for(key: Key) -> Option<Action> {
    Some(match key {
        k if k == Key::W || k == Key::UP => Action::Forward,
        k if k == Key::S || k == Key::DOWN => Action::Back,
        k if k == Key::A => Action::Left,
        k if k == Key::D => Action::Right,
        k if k == Key::SPACE || k == Key::E || k == Key::PAGE_UP => Action::Up,
        k if k == Key::C || k == Key::Q || k == Key::PAGE_DOWN => Action::Down,
        k if k == Key::LEFT => Action::TurnLeft,
        k if k == Key::RIGHT => Action::TurnRight,
        k if k == Key::SHIFT => Action::Fast,
        _ => return None,
    })
}

/// The set of held movements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held(u16);

impl Held {
    pub fn press(&mut self, a: Action) {
        self.0 |= a.bit();
    }

    pub fn release(&mut self, a: Action) {
        self.0 &= !a.bit();
    }

    pub fn clear(&mut self) {
        self.0 = 0;
    }

    pub fn has(self, a: Action) -> bool {
        self.0 & a.bit() != 0
    }

    /// Whether anything other than Shift is held.
    pub fn moving(self) -> bool {
        self.0 & !Action::Fast.bit() != 0
    }
}

/// The camera and how fast it flies.
pub struct Flyer {
    pub camera: Camera,
    /// Metres per second at walking pace (Shift is four times this).
    pub speed: f32,
}

/// The lowest the eye may go above the ground, metres.
pub const EYE: f32 = 1.7;
const MAX_ALTITUDE: f32 = 700.0;
const TURN_RATE: f32 = 1.6;
/// Radians per mouse pixel.
const LOOK: f32 = 0.0045;

impl Flyer {
    pub fn new(camera: Camera) -> Flyer {
        Flyer {
            camera,
            speed: 22.0,
        }
    }

    /// Moves the camera for `dt` seconds of `held`; `ground(x, z)` keeps it
    /// above the surface and `size` inside the course (with a margin).
    /// Returns whether it moved.
    pub fn update(
        &mut self,
        held: Held,
        dt: f32,
        size: f32,
        ground: impl Fn(f32, f32) -> f32,
    ) -> bool {
        if !held.moving() {
            return false;
        }
        let speed = self.speed * if held.has(Action::Fast) { 4.0 } else { 1.0 } * dt;
        let cam = &mut self.camera;
        let axis = |plus: Action, minus: Action| {
            f32::from(u8::from(held.has(plus))) - f32::from(u8::from(held.has(minus)))
        };
        cam.yaw += axis(Action::TurnRight, Action::TurnLeft) * TURN_RATE * dt;
        let forward = cam.forward();
        let right = Vec3::new(cam.yaw.cos(), 0.0, cam.yaw.sin());
        let step = forward * (axis(Action::Forward, Action::Back) * speed)
            + right * (axis(Action::Right, Action::Left) * speed)
            + Vec3::new(0.0, axis(Action::Up, Action::Down) * speed, 0.0);
        let mut eye = cam.eye + step;
        eye.x = eye.x.clamp(-150.0, size + 150.0);
        eye.z = eye.z.clamp(-150.0, size + 150.0);
        let floor = ground(eye.x.clamp(0.0, size - 1.0), eye.z.clamp(0.0, size - 1.0)) + EYE;
        eye.y = eye.y.clamp(floor, floor.max(MAX_ALTITUDE));
        cam.eye = eye;
        true
    }

    /// Turns the view by a mouse drag of (`dx`, `dy`) pixels.
    pub fn look(&mut self, dx: f32, dy: f32) {
        self.camera.yaw += dx * LOOK;
        self.camera.pitch = (self.camera.pitch - dy * LOOK).clamp(-1.45, 1.45);
    }

    /// Slides the camera sideways and up by a drag of (`dx`, `dy`) pixels.
    pub fn pan(&mut self, dx: f32, dy: f32) {
        let k = (self.speed * 0.02).max(0.1);
        let right = Vec3::new(self.camera.yaw.cos(), 0.0, self.camera.yaw.sin());
        self.camera.eye = self.camera.eye + right * (-dx * k) + Vec3::new(0.0, dy * k, 0.0);
    }

    /// The wheel changes the flying speed, a notch at a time.
    pub fn wheel(&mut self, notches: f32) {
        self.speed = (self.speed * 1.25f32.powf(notches)).clamp(2.0, 400.0);
    }

    /// Stand behind `hole`'s back tee at eye height, looking down the hole.
    pub fn to_tee(&mut self, hole: &Hole, ground: impl Fn(f32, f32) -> f32) {
        let heading = hole.tee_heading();
        let p = hole.tees[0].position - heading * 7.0;
        self.camera.eye = Vec3::new(p.x, ground(p.x, p.y) + EYE, p.y);
        self.camera.yaw = yaw_of(heading);
        self.camera.pitch = -0.03;
    }
}

/// The yaw that looks along ground direction `dir`.
pub fn yaw_of(dir: Vec2) -> f32 {
    dir.x.atan2(-dir.y)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flyer() -> Flyer {
        Flyer::new(Camera::new(Vec3::new(100.0, 10.0, 100.0), 0.0, 0.0))
    }

    #[test]
    fn forward_goes_where_it_looks_and_shift_is_faster() {
        let mut f = flyer();
        let mut held = Held::default();
        held.press(Action::Forward);
        assert!(f.update(held, 1.0, 1024.0, |_, _| 0.0));
        let slow = 100.0 - f.camera.eye.z;
        assert!(
            slow > 20.0 && (f.camera.eye.x - 100.0).abs() < 1e-3,
            "north is -z"
        );
        held.press(Action::Fast);
        let before = f.camera.eye.z;
        f.update(held, 1.0, 1024.0, |_, _| 0.0);
        assert!((before - f.camera.eye.z) > slow * 3.5);
        held.clear();
        assert!(!f.update(held, 1.0, 1024.0, |_, _| 0.0));
    }

    #[test]
    fn the_ground_and_the_edges_hold_the_camera() {
        let mut f = flyer();
        let mut held = Held::default();
        held.press(Action::Down);
        f.update(held, 10.0, 1024.0, |_, _| 5.0);
        assert!((f.camera.eye.y - (5.0 + EYE)).abs() < 1e-4);
        held = Held::default();
        held.press(Action::Left);
        f.update(held, 100.0, 1024.0, |_, _| 0.0);
        assert!(f.camera.eye.x >= -150.0);
    }

    #[test]
    fn strafing_right_goes_east_when_facing_north() {
        let mut f = flyer();
        let mut held = Held::default();
        held.press(Action::Right);
        f.update(held, 1.0, 1024.0, |_, _| 0.0);
        assert!(f.camera.eye.x > 110.0);
    }

    #[test]
    fn keys_map_and_look_clamps() {
        assert_eq!(action_for(Key::W), Some(Action::Forward));
        assert_eq!(action_for(Key::PAGE_DOWN), Some(Action::Down));
        assert_eq!(action_for(Key::G), None);
        let mut f = flyer();
        f.look(0.0, -100_000.0);
        assert!(f.camera.pitch <= 1.45);
        f.wheel(100.0);
        assert_eq!(f.speed, 400.0);
    }
}
