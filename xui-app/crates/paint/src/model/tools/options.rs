#![forbid(unsafe_code)]

//! Each drawing tool's options as plain data, so the options bar can bind to
//! them without the model knowing about widgets.

/// The widest brush the stroke engine accepts, in pixels.
pub const MAX_BRUSH_WIDTH: u32 = 500;

/// The Paintbrush and Eraser options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrushOptions {
    /// Diameter in pixels, `1..=MAX_BRUSH_WIDTH`.
    pub width: u32,
    /// Edge hardness in percent: 100 is a crisp disc, 0 a falloff from the
    /// centre to the rim.
    pub hardness: u8,
    /// Antialiasing; off gives hard 0/255 coverage.
    pub antialias: bool,
}

impl Default for BrushOptions {
    fn default() -> Self {
        BrushOptions {
            width: 10,
            hardness: 100,
            antialias: true,
        }
    }
}

impl BrushOptions {
    /// The Pencil: one pixel, aliased, exact colour.
    pub const PENCIL: BrushOptions = BrushOptions {
        width: 1,
        hardness: 100,
        antialias: false,
    };

    /// The same options with every field forced into range.
    pub fn clamped(self) -> BrushOptions {
        BrushOptions {
            width: self.width.clamp(1, MAX_BRUSH_WIDTH),
            hardness: self.hardness.min(100),
            antialias: self.antialias,
        }
    }
}

/// What a shape paints.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FillMode {
    /// The outline only, in the primary colour.
    #[default]
    Outline,
    /// The inside only, in the secondary colour.
    Fill,
    /// Both: primary outline over a secondary fill.
    Both,
}

impl FillMode {
    /// Whether the outline is drawn.
    pub const fn has_outline(self) -> bool {
        matches!(self, FillMode::Outline | FillMode::Both)
    }

    /// Whether the inside is filled.
    pub const fn has_fill(self) -> bool {
        matches!(self, FillMode::Fill | FillMode::Both)
    }
}

/// A line's arrow heads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Arrow {
    /// A plain line.
    #[default]
    None,
    /// A head at the start point.
    Start,
    /// A head at the end point.
    End,
    /// Heads at both ends.
    Both,
}

/// The Line, Rectangle, Rounded Rectangle and Ellipse options.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeOptions {
    /// Outline width in pixels.
    pub width: f32,
    /// Outline, fill or both.
    pub mode: FillMode,
    /// Corner radius of a rounded rectangle, in pixels.
    pub radius: f32,
    /// Arrow heads (lines only).
    pub arrow: Arrow,
    /// Antialiasing; off gives hard 0/255 coverage.
    pub antialias: bool,
}

impl Default for ShapeOptions {
    fn default() -> Self {
        ShapeOptions {
            width: 2.0,
            mode: FillMode::Outline,
            radius: 0.0,
            arrow: Arrow::None,
            antialias: true,
        }
    }
}

/// Which pixels the Paint Bucket compares.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sample {
    /// The active layer alone.
    #[default]
    Layer,
    /// The flattened image.
    Image,
}

/// Which pixels the Paint Bucket may fill.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FillArea {
    /// Only the region connected to the click.
    #[default]
    Contiguous,
    /// Every matching pixel.
    Global,
}

/// The Paint Bucket (and later Magic Wand) options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FillOptions {
    /// How far a pixel may differ from the seed, 0..=100 percent of the
    /// largest RGBA distance.
    pub tolerance: u8,
    /// Contiguous or global.
    pub area: FillArea,
    /// Compare the layer or the flattened image.
    pub sample: Sample,
}

/// The gradient shapes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GradientKind {
    /// Primary at the start, secondary at the end, along the drag.
    #[default]
    Linear,
    /// Primary at the start, secondary at the end on both sides of it.
    LinearReflected,
    /// Primary at the centre, secondary at the drag's length.
    Radial,
    /// As radial, with square contours turned to the drag.
    Diamond,
}

/// The Gradient options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GradientOptions {
    /// The shape.
    pub kind: GradientKind,
}

/// How many pixels the Color Picker averages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SampleSize {
    /// The pixel under the pointer.
    #[default]
    One,
    /// A 3x3 average.
    Three,
    /// A 5x5 average.
    Five,
}

impl SampleSize {
    /// The window's side in pixels.
    pub const fn side(self) -> i32 {
        match self {
            SampleSize::One => 1,
            SampleSize::Three => 3,
            SampleSize::Five => 5,
        }
    }
}

/// The Color Picker options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PickerOptions {
    /// One pixel, 3x3 or 5x5.
    pub size: SampleSize,
    /// The layer or the flattened image.
    pub sample: Sample,
    /// Return to the previous tool after a pick.
    pub switch_back: bool,
}
