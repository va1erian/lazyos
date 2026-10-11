//! The clubs: how fast, how high and how much spin a full swing gives the
//! ball. The numbers are an average amateur's, tuned so a full shot carries
//! the distance in `carry_m` on flat ground in still air (the tests hold
//! them to it).

use crate::math::{Vec2, Vec3};

use super::ball::Launch;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Club {
    pub name: &'static str,
    /// Ball speed of a full swing, m/s.
    pub speed: f32,
    /// Launch angle, degrees.
    pub launch_deg: f32,
    /// Backspin, rpm.
    pub backspin: f32,
    /// The full-swing carry on flat ground in still air, metres.
    pub carry_m: f32,
}

impl Club {
    /// The ball leaving `position` along `heading` at `power` (0..=1.2 of
    /// a full swing); `sidespin` in rpm curves it (positive: right).
    pub fn launch(&self, position: Vec3, heading: Vec2, power: f32, sidespin: f32) -> Launch {
        let power = power.clamp(0.0, 1.2);
        Launch {
            position,
            heading,
            speed: self.speed * power,
            angle: self.launch_deg.to_radians(),
            backspin: self.backspin * power,
            sidespin,
        }
    }
}

/// Longest to shortest.
pub const CLUBS: [Club; 11] = [
    club("Driver", 58.6, 12.0, 2600.0, 200.0),
    club("3 Wood", 54.0, 13.0, 3300.0, 185.0),
    club("5 Wood", 50.5, 14.5, 3900.0, 170.0),
    club("4 Iron", 47.5, 15.0, 4300.0, 155.0),
    club("5 Iron", 45.9, 16.0, 4800.0, 148.0),
    club("6 Iron", 44.1, 17.5, 5400.0, 140.0),
    club("7 Iron", 41.8, 19.0, 6000.0, 130.0),
    club("8 Iron", 39.5, 21.0, 6700.0, 120.0),
    club("9 Iron", 36.7, 24.0, 7400.0, 108.0),
    club("Pitching Wedge", 33.8, 27.0, 8000.0, 95.0),
    club("Sand Wedge", 28.0, 33.0, 8500.0, 70.0),
];

const fn club(
    name: &'static str,
    speed: f32,
    launch_deg: f32,
    backspin: f32,
    carry_m: f32,
) -> Club {
    Club {
        name,
        speed,
        launch_deg,
        backspin,
        carry_m,
    }
}
