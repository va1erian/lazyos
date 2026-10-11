//! What each material does to a ball (bounce, roll, grip) and to a swing
//! from it (the lie). One table each, so the feel of the game is tuned in
//! one place.

use crate::course::Material;

use super::clubs::{Club, BAG};

/// How a surface treats a ball that lands or rolls on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Surface {
    /// Share of the speed into the surface that comes back out.
    pub restitution: f32,
    /// Share of the speed along the surface kept through a bounce.
    pub impact_keep: f32,
    /// How well it holds backspin (1: a green that checks a wedge).
    pub grip: f32,
    /// Rolling resistance as a fraction of gravity: deceleration is
    /// `roll * g` on the flat. A fast green is about 0.05.
    pub roll: f32,
}

const fn s(restitution: f32, impact_keep: f32, grip: f32, roll: f32) -> Surface {
    Surface {
        restitution,
        impact_keep,
        grip,
        roll,
    }
}

pub fn of(material: Material) -> Surface {
    match material {
        Material::Green => s(0.30, 0.60, 1.0, 0.055),
        Material::Fringe => s(0.30, 0.60, 0.8, 0.09),
        Material::TeeBox => s(0.35, 0.65, 0.7, 0.10),
        Material::Fairway => s(0.35, 0.65, 0.7, 0.11),
        Material::FirstCut => s(0.25, 0.50, 0.45, 0.20),
        Material::Rough | Material::OutOfBounds => s(0.18, 0.35, 0.25, 0.40),
        Material::DeepRough | Material::Woodland => s(0.10, 0.20, 0.10, 0.80),
        Material::Sand => s(0.04, 0.10, 0.0, 1.4),
        Material::WasteArea => s(0.08, 0.25, 0.10, 0.60),
        Material::CartPath => s(0.55, 0.85, 0.20, 0.035),
        Material::Rock => s(0.60, 0.80, 0.10, 0.08),
        Material::Water => s(0.0, 0.0, 0.0, 1.0),
    }
}

/// What lying on a material does to the next swing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lie {
    /// Share of the swing's power that reaches the ball.
    pub power: f32,
    /// Share of the club's backspin.
    pub spin: f32,
    /// Direction error (radians, either way) from a clean-looking swing.
    pub scatter: f32,
}

pub fn lie(material: Material, club: &Club) -> Lie {
    let wedge = BAG
        .iter()
        .position(|c| c.name == club.name)
        .is_some_and(|i| i >= 9);
    let l = |power: f32, spin: f32, scatter: f32| Lie {
        power,
        spin,
        scatter,
    };
    match material {
        Material::TeeBox | Material::Fairway | Material::Green => l(1.0, 1.0, 0.0),
        Material::Fringe => l(0.97, 0.9, 0.005),
        Material::FirstCut => l(0.95, 0.8, 0.01),
        Material::Rough | Material::OutOfBounds => l(0.85, 0.5, 0.03),
        Material::DeepRough => l(0.65, 0.25, 0.06),
        Material::Woodland => l(0.6, 0.2, 0.07),
        // A wedge gets a ball out of sand; anything else half-way.
        Material::Sand if wedge => l(0.95, 0.1, 0.02),
        Material::Sand => l(0.65, 0.1, 0.05),
        Material::WasteArea => l(0.85, 0.4, 0.03),
        Material::CartPath => l(1.0, 0.7, 0.01),
        Material::Rock => l(0.7, 0.5, 0.04),
        Material::Water => l(0.0, 0.0, 0.0),
    }
}
