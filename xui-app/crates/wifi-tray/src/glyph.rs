//! The tray icon: a Wi-Fi fan (a dot and three arcs) drawn as an alpha mask.
//!
//! Four lit segments are four bars of signal. A segment that is not lit is
//! drawn faint, as on a phone, so the fan keeps its shape at every strength.
//! The other states have their own look: disconnected is all faint,
//! connecting lights the fan step by step (the applet advances the phase on
//! every look) at a lower alpha than a settled connection, and unavailable
//! is the faint fan struck through.
//!
//! The mask is 32 x 32, the largest a cell needs (16 dp at 2x); the shell
//! scales it for 1x. Pixels are straight RGBA with white colour: only the
//! alpha is used.

/// Side of the mask in pixels.
pub const SIDE: u32 = 32;
/// Samples per axis inside a pixel.
const SAMPLES: u32 = 4;

/// Alpha of a lit segment of a settled connection.
const LIT: u32 = 255;
/// Alpha of a lit segment while connecting.
const JOINING: u32 = 170;
/// Alpha of a segment that is not lit.
const FAINT: u32 = 70;

/// The fan's centre, where the dot sits.
const CENTRE: (f32, f32) = (16.0, 26.0);
const DOT_RADIUS: f32 = 2.5;
/// The arcs' radii, innermost first, and their stroke.
const ARC_RADII: [f32; 3] = [7.5, 12.5, 17.5];
const STROKE: f32 = 2.6;
/// How far from straight up an arc reaches on each side (degrees).
const FAN_HALF_ANGLE: f32 = 47.0;
/// The strike-through of an unavailable radio, corner to corner.
const SLASH: ((f32, f32), (f32, f32)) = ((5.0, 5.0), (27.0, 27.0));
/// Half the width of the strike, and of the gap cleared around it.
const SLASH_HALF: f32 = 1.4;
const SLASH_GAP: f32 = 3.4;

/// Segments of the fan, innermost first: the dot, then the arcs.
pub const SEGMENTS: u8 = 4;

/// What the icon shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    /// Connected with this many bars lit (1 to 4; no signal still shows the
    /// dot).
    Bars(u8),
    /// A radio that is not joined to anything.
    Disconnected,
    /// Joining a network; the phase (0 to 3) lights that many arcs.
    Connecting(u8),
    /// No Wi-Fi radio is usable (the service is down, the card is gone).
    Unavailable,
}

impl Glyph {
    /// Connected glyph for a signal of `bars` (0 to 4): a connection always
    /// shows at least the dot, so it never looks like the disconnected icon.
    pub fn connected(bars: u8) -> Glyph {
        Glyph::Bars(bars.clamp(1, SEGMENTS))
    }

    /// A short name for the serial evidence (`NETSTATUS:ICON bars3`).
    pub fn name(self) -> String {
        match self {
            Glyph::Bars(n) => format!("bars{n}"),
            Glyph::Disconnected => String::from("disconnected"),
            Glyph::Connecting(phase) => format!("connecting{phase}"),
            Glyph::Unavailable => String::from("unavailable"),
        }
    }

    /// How many segments are lit and at what alpha.
    fn lit(self) -> (u8, u32) {
        match self {
            Glyph::Bars(n) => (n.min(SEGMENTS), LIT),
            Glyph::Connecting(phase) => (phase.min(SEGMENTS - 1) + 1, JOINING),
            Glyph::Disconnected | Glyph::Unavailable => (0, LIT),
        }
    }

    /// The glyph as an alpha mask.
    pub fn mask(self) -> Mask {
        let (lit, strong) = self.lit();
        let strike = self == Glyph::Unavailable;
        let mut rgba = Vec::with_capacity((SIDE * SIDE * 4) as usize);
        for y in 0..SIDE {
            for x in 0..SIDE {
                let mut total = 0;
                for sy in 0..SAMPLES {
                    for sx in 0..SAMPLES {
                        let px = x as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
                        let py = y as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
                        total += sample(px, py, lit, strong, strike);
                    }
                }
                let alpha = total / (SAMPLES * SAMPLES);
                rgba.extend_from_slice(&[255, 255, 255, alpha as u8]);
            }
        }
        Mask {
            width: SIDE,
            height: SIDE,
            rgba,
        }
    }
}

/// An alpha-only image, straight RGBA8 (`os.lazy.shell.tray.v1` `Icon.mask`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mask {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Mask {
    /// The wire icon.
    pub fn icon(&self) -> trayclient::wire::Icon {
        trayclient::wire::Icon {
            mask: Some(trayclient::wire::Image {
                width: self.width,
                height: self.height,
                data: self.rgba.clone(),
            }),
            ..trayclient::wire::Icon::default()
        }
    }

    /// The alpha at (`x`, `y`).
    pub fn alpha(&self, x: u32, y: u32) -> u8 {
        self.rgba[((y * self.width + x) * 4 + 3) as usize]
    }

    /// Total alpha, a measure of how much ink the icon has.
    pub fn ink(&self) -> u64 {
        self.rgba.chunks_exact(4).map(|p| u64::from(p[3])).sum()
    }
}

/// The alpha (0 to 255) the glyph has at one sample point.
fn sample(px: f32, py: f32, lit: u8, strong: u32, strike: bool) -> u32 {
    if strike {
        let away = distance_to_segment((px, py), SLASH.0, SLASH.1);
        if away <= SLASH_HALF {
            return LIT;
        }
        if away <= SLASH_GAP {
            return 0;
        }
    }
    match segment_at(px, py) {
        Some(index) if index < lit => strong,
        Some(_) => FAINT,
        None => 0,
    }
}

/// Which segment of the fan covers the point, if any.
fn segment_at(px: f32, py: f32) -> Option<u8> {
    let (dx, dy) = (px - CENTRE.0, py - CENTRE.1);
    let radius = (dx * dx + dy * dy).sqrt();
    if radius <= DOT_RADIUS {
        return Some(0);
    }
    // Angle from straight up, in degrees; the fan opens upwards.
    let angle = dx.atan2(-dy).to_degrees().abs();
    if dy >= 0.0 || angle > FAN_HALF_ANGLE {
        return None;
    }
    ARC_RADII
        .iter()
        .position(|r| (radius - r).abs() <= STROKE / 2.0)
        .map(|arc| arc as u8 + 1)
}

fn distance_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let length_sq = abx * abx + aby * aby;
    let t = (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / length_sq).clamp(0.0, 1.0);
    let (cx, cy) = (a.0 + t * abx, a.1 + t * aby);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests;
