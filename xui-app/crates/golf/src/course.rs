//! The generated course: the heightfield, one material per cell, the objects
//! on it and the 18 holes (docs/golf-course-generator.md, "Output types").

use crate::field::Field;
use crate::math::Vec2;

/// What covers a 1 m cell, from the most to the least specific.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Material {
    Green = 0,
    Fringe,
    TeeBox,
    Fairway,
    FirstCut,
    Rough,
    DeepRough,
    Sand,
    WasteArea,
    Water,
    Woodland,
    CartPath,
    /// Bare rock on a crag face.
    Rock,
    OutOfBounds,
}

impl Material {
    pub const ALL: [Material; 14] = [
        Material::Green,
        Material::Fringe,
        Material::TeeBox,
        Material::Fairway,
        Material::FirstCut,
        Material::Rough,
        Material::DeepRough,
        Material::Sand,
        Material::WasteArea,
        Material::Water,
        Material::Woodland,
        Material::CartPath,
        Material::Rock,
        Material::OutOfBounds,
    ];

    pub fn from_u8(value: u8) -> Material {
        Material::ALL[usize::from(value).min(Material::ALL.len() - 1)]
    }

    /// Whether the ball is in play on it (the routing keeps play here).
    pub fn is_short_grass(self) -> bool {
        matches!(
            self,
            Material::Green | Material::Fringe | Material::TeeBox | Material::Fairway
        )
    }

    pub fn name(self) -> &'static str {
        match self {
            Material::Green => "green",
            Material::Fringe => "fringe",
            Material::TeeBox => "tee",
            Material::Fairway => "fairway",
            Material::FirstCut => "first cut",
            Material::Rough => "rough",
            Material::DeepRough => "deep rough",
            Material::Sand => "bunker",
            Material::WasteArea => "waste area",
            Material::Water => "water",
            Material::Woodland => "woodland",
            Material::CartPath => "cart path",
            Material::Rock => "rock",
            Material::OutOfBounds => "out of bounds",
        }
    }
}

/// The landform the site terrain is built around.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Archetype {
    /// Low dunes on sandy soil, few trees, fescue and waste areas.
    Links,
    /// Rolling hills and woodland.
    Parkland,
    /// High relief and terraces.
    Mountain,
}

impl Archetype {
    pub const ALL: [Archetype; 3] = [Archetype::Links, Archetype::Parkland, Archetype::Mountain];

    /// How much relief is normal for the landform, relative to parkland:
    /// a links hole that rolls a few metres is as lively as a mountain hole
    /// that drops twenty.
    pub fn relief_scale(self) -> f32 {
        match self {
            Archetype::Links => 0.55,
            Archetype::Parkland => 0.85,
            Archetype::Mountain => 1.3,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Archetype::Links => "Links",
            Archetype::Parkland => "Parkland",
            Archetype::Mountain => "Mountain",
        }
    }
}

/// A tree species: its look comes from the renderer's sprite set.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Species {
    Oak = 0,
    Pine,
    Willow,
    Poplar,
    UmbrellaPine,
    Gorse,
}

impl Species {
    pub const COUNT: usize = 6;
}

/// Something standing on the course.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ObjectKind {
    Tree {
        species: Species,
        /// Which of the species' sprite variants.
        variant: u8,
        mirrored: bool,
    },
    Flagstick {
        hole: u8,
    },
    /// Tee set index: 0 back (black), 1 middle (white), 2 forward (yellow), ...
    TeeMarker {
        set: u8,
    },
    /// Metres to the green centre: 200, 150 or 100.
    YardageMarker {
        metres: u16,
    },
    OutOfBoundsStake,
    Bench,
    BallWasher,
    /// A footbridge where the cart path crosses water; `angle` is its run.
    Bridge {
        angle: f32,
        length: f32,
    },
    /// The clubhouse; `angle` is the facade's facing.
    Clubhouse {
        angle: f32,
    },
}

/// One object: what it is, where its base stands and how tall it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Object {
    pub kind: ObjectKind,
    pub position: Vec2,
    /// Ground height at the base, metres.
    pub base: f32,
    pub height: f32,
}

/// A closed outline on the ground (a green, a bunker).
pub type Polygon = Vec<Vec2>;

/// One set of tees (back, middle, forward, ...).
#[derive(Clone, Debug, PartialEq)]
pub struct TeeSet {
    pub name: &'static str,
    pub position: Vec2,
    /// Playing length from this set along the centerline, metres.
    pub length_m: f32,
}

/// One hole as the routing and the layout made it.
#[derive(Clone, Debug, PartialEq)]
pub struct Hole {
    pub number: u8,
    pub par: u8,
    /// Back to front.
    pub tees: Vec<TeeSet>,
    /// From the back tee to the green centre.
    pub centerline: Vec<Vec2>,
    pub green: Polygon,
    pub green_center: Vec2,
    pub pin: Vec2,
    pub bunkers: Vec<Polygon>,
    /// Distances along the centerline of the scratch and bogey landing zones.
    pub landing_zones: [f32; 2],
    pub length_m: f32,
    pub effective_length_m: f32,
    /// Expected strokes for the scratch and the bogey player models.
    pub scratch_expected: f32,
    pub bogey_expected: f32,
    /// Stroke index, 1 (hardest) to 18.
    pub handicap_index: u8,
}

impl Hole {
    /// The direction of play off the back tee.
    pub fn tee_heading(&self) -> Vec2 {
        match self.centerline.as_slice() {
            [a, b, ..] => (*b - *a).normalized(),
            _ => Vec2::new(1.0, 0.0),
        }
    }

    /// The line of play's overall direction, tee to green.
    pub fn axis(&self) -> Vec2 {
        match self.centerline.as_slice() {
            [a, .., b] => (*b - *a).normalized(),
            _ => Vec2::new(1.0, 0.0),
        }
    }
}

/// A generated course.
#[derive(Clone, Debug)]
pub struct Course {
    /// The seed the course was searched from.
    pub seed: u64,
    /// The seed of the site the search chose.
    pub site_seed: u64,
    /// The chosen routing's quality score (see `gen::quality`), about 0..10.
    pub quality: f32,
    pub archetype: Archetype,
    pub name: String,
    pub cell_size_m: f32,
    pub width: u32,
    pub height: u32,
    /// Metres, one per cell, sampled at cell centres.
    pub elevation: Field<f32>,
    pub material: Field<Material>,
    /// Distance from each cell to the nearest cell of another material,
    /// metres (0.5 on a boundary cell), so edges can be shaded smoothly.
    pub edge_sdf: Field<f32>,
    /// 0 dry .. 1 wet; drives rough density and tree species.
    pub moisture: Field<f32>,
    /// The hole whose corridor a cell lies in (255: none), for the
    /// renderer's per-hole mowing direction.
    pub hole_of: Field<u8>,
    /// The flat water surface height for each water cell's body.
    pub water_level: Field<f32>,
    pub objects: Vec<Object>,
    pub holes: Vec<Hole>,
    pub clubhouse: Vec2,
    pub par: u8,
    pub rating: f32,
    pub slope: u16,
}

impl Course {
    /// The material under a world point.
    pub fn material_at(&self, x: f32, z: f32) -> Material {
        self.material.at(x, z)
    }

    /// The ground (or water surface) height under a world point.
    pub fn ground_at(&self, x: f32, z: f32) -> f32 {
        let ground = self.elevation.sample(x, z);
        if self.material_at(x, z) == Material::Water {
            ground.max(self.water_level.at(x, z))
        } else {
            ground
        }
    }

    pub fn total_length_m(&self) -> f32 {
        self.holes.iter().map(|h| h.length_m).sum()
    }
}
