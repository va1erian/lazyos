//! The 256-colour palette: 16-entry ramps per material, Bayer dithering
//! between ramp steps, a haze lookup table (Doom's COLORMAP idea) and the
//! palette-cycled water.

/// Ramp bases (each ramp is 16 entries, dark to light).
pub const GREY: u8 = 0;
pub const SKY: u8 = 16;
pub const FAIRWAY: u8 = 32;
pub const GREEN: u8 = 48;
pub const ROUGH: u8 = 64;
pub const FESCUE: u8 = 80;
pub const SAND: u8 = 96;
/// 4 brightness levels x 4 wave phases; the phases cycle.
pub const WATER: u8 = 112;
pub const FOLIAGE_A: u8 = 128;
pub const FOLIAGE_B: u8 = 144;
pub const STONE: u8 = 160;
pub const HILLS: u8 = 176;
pub const FOREST_FLOOR: u8 = 192;
pub const WALL: u8 = 208;
pub const ROOF: u8 = 224;

/// Single colours.
pub const WHITE: u8 = 240;
pub const RED: u8 = 241;
pub const YELLOW: u8 = 242;
pub const BLUE: u8 = 243;
pub const BLACK: u8 = 244;
pub const FLOWER: u8 = 245;
pub const CLOUD: u8 = 246;
pub const CLOUD_LIGHT: u8 = 247;
pub const MARKER_GREEN: u8 = 248;
pub const WOOD: u8 = 249;
/// Four still water shades (the animated ones' average), for water too far
/// off to show ripples: animated, a whole distant lake would pulse at once.
pub const CALM_WATER: u8 = 250;

/// One full cycle of the water's palette animation, seconds.
pub const WATER_CYCLE: f32 = 2.4;
/// How far a ripple highlight lifts the water's colour at its peak.
const RIPPLE_LIFT: f32 = 18.0;
const WATER_LEVELS: [[u8; 3]; 4] = [[22, 54, 98], [36, 80, 140], [56, 108, 172], [92, 146, 204]];

/// Haze levels in the lookup table.
pub const FOG_LEVELS: usize = 16;

pub const BAYER4: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// A ramp step from a light value (`0..=255`, 16 sub-steps per entry),
/// dithered with the Bayer matrix between adjacent entries.
#[inline]
pub fn shade_index(ramp: u8, light: i32, x: usize, y: usize) -> u8 {
    let light = light.clamp(0, 15 * 16);
    let level = (light >> 4) as u8;
    let frac = (light & 15) as u8;
    let bump = u8::from(frac > BAYER4[y & 3][x & 3]);
    ramp + (level + bump).min(15)
}

/// The palette, its RGBA words and its haze table.
pub struct Palette {
    pub rgb: [[u8; 3]; 256],
    /// `fog[level * 256 + index]`: `index` hazed `level / 15` of the way.
    pub fog: Vec<u8>,
    pub haze: [u8; 3],
}

impl Palette {
    pub fn new() -> Palette {
        let mut rgb = [[0u8; 3]; 256];
        ramp(&mut rgb, GREY, [0, 0, 0], [128, 128, 128], [255, 255, 255]);
        ramp(
            &mut rgb,
            SKY,
            [206, 220, 232],
            [140, 178, 222],
            [58, 104, 190],
        );
        ramp(
            &mut rgb,
            FAIRWAY,
            [18, 52, 12],
            [76, 146, 46],
            [170, 218, 98],
        );
        ramp(
            &mut rgb,
            GREEN,
            [22, 70, 30],
            [88, 172, 74],
            [180, 236, 136],
        );
        ramp(&mut rgb, ROUGH, [14, 40, 12], [54, 106, 36], [128, 168, 66]);
        ramp(
            &mut rgb,
            FESCUE,
            [46, 50, 18],
            [128, 130, 60],
            [214, 206, 128],
        );
        ramp(
            &mut rgb,
            SAND,
            [112, 92, 56],
            [212, 190, 136],
            [252, 244, 212],
        );
        ramp(
            &mut rgb,
            FOLIAGE_A,
            [10, 30, 8],
            [50, 100, 32],
            [150, 188, 78],
        );
        ramp(
            &mut rgb,
            FOLIAGE_B,
            [8, 28, 22],
            [36, 82, 52],
            [112, 158, 104],
        );
        ramp(
            &mut rgb,
            STONE,
            [40, 30, 22],
            [136, 126, 110],
            [228, 222, 206],
        );
        ramp(
            &mut rgb,
            HILLS,
            [78, 104, 118],
            [126, 150, 166],
            [182, 200, 212],
        );
        ramp(
            &mut rgb,
            FOREST_FLOOR,
            [10, 26, 8],
            [36, 68, 24],
            [92, 124, 52],
        );
        ramp(
            &mut rgb,
            WALL,
            [92, 78, 62],
            [196, 182, 156],
            [248, 240, 222],
        );
        ramp(&mut rgb, ROOF, [58, 20, 14], [148, 60, 40], [222, 142, 112]);
        water(&mut rgb, 0.0);
        for (level, base) in WATER_LEVELS.iter().enumerate() {
            let calm = base.map(|v| (f32::from(v) + RIPPLE_LIFT * 0.5).min(255.0) as u8);
            rgb[usize::from(CALM_WATER) + level] = calm;
        }
        let singles: [(u8, [u8; 3]); 10] = [
            (WHITE, [250, 250, 250]),
            (RED, [220, 30, 30]),
            (YELLOW, [240, 210, 40]),
            (BLUE, [40, 80, 220]),
            (BLACK, [16, 16, 16]),
            (FLOWER, [236, 196, 40]),
            (CLOUD, [222, 230, 240]),
            (CLOUD_LIGHT, [246, 248, 252]),
            (MARKER_GREEN, [30, 150, 60]),
            (WOOD, [120, 82, 48]),
        ];
        for (i, c) in singles {
            rgb[usize::from(i)] = c;
        }
        let haze = rgb[usize::from(SKY) + 1];
        let fog = fog_table(&rgb, haze);
        Palette { rgb, fog, haze }
    }

    /// The water entries `seconds` into the animation: each phase's
    /// highlight swells and fades on a slow sine, a quarter cycle behind
    /// the last, so the ripples drift smoothly rather than blink.
    pub fn animate_water(&mut self, seconds: f32) {
        water(&mut self.rgb, seconds);
    }

    /// Opaque RGBA bytes for every entry, as one little-endian word each.
    pub fn words(&self) -> [u32; 256] {
        let mut out = [0u32; 256];
        for (w, c) in out.iter_mut().zip(self.rgb.iter()) {
            *w = u32::from_le_bytes([c[0], c[1], c[2], 255]);
        }
        out
    }
}

impl Default for Palette {
    fn default() -> Palette {
        Palette::new()
    }
}

/// Fills a 16-entry ramp: `dark` to `mid` over the first half, `mid` to
/// `light` over the second.
fn ramp(rgb: &mut [[u8; 3]; 256], base: u8, dark: [u8; 3], mid: [u8; 3], light: [u8; 3]) {
    for k in 0..16 {
        let (a, b, t) = if k < 8 {
            (dark, mid, k as f32 / 8.0)
        } else {
            (mid, light, (k - 8) as f32 / 7.0)
        };
        let mut c = [0u8; 3];
        for ch in 0..3 {
            c[ch] = (a[ch] as f32 + (b[ch] as f32 - a[ch] as f32) * t).round() as u8;
        }
        rgb[usize::from(base) + k] = c;
    }
}

/// The water ramp at `seconds`: 4 brightness levels, each with 4 phases of
/// a ripple highlight on a sine, a quarter cycle apart.
fn water(rgb: &mut [[u8; 3]; 256], seconds: f32) {
    let t = seconds / WATER_CYCLE;
    for (level, base) in WATER_LEVELS.iter().enumerate() {
        for phase in 0..4 {
            let angle = std::f32::consts::TAU * (t + phase as f32 / 4.0);
            let lift = RIPPLE_LIFT * (0.5 - 0.5 * angle.cos());
            let c = base.map(|v| (f32::from(v) + lift).round().clamp(0.0, 255.0) as u8);
            rgb[usize::from(WATER) + level * 4 + phase] = c;
        }
    }
}

/// For each haze level and palette entry, the nearest entry to the colour
/// hazed that far. Water entries are never a target (they would cycle) and
/// keep their own index until the haze is half way.
fn fog_table(rgb: &[[u8; 3]; 256], haze: [u8; 3]) -> Vec<u8> {
    let targets: Vec<usize> = (0..256)
        .filter(|&i| !(112..128).contains(&i) && i != 0)
        .collect();
    let mut out = vec![0u8; FOG_LEVELS * 256];
    for level in 0..FOG_LEVELS {
        let t = level as f32 / (FOG_LEVELS - 1) as f32 * 0.92;
        for index in 0..256 {
            let c = rgb[index];
            if level < 6 && (112..128).contains(&index) {
                out[level * 256 + index] = index as u8;
                continue;
            }
            let want = [0, 1, 2].map(|ch| c[ch] as f32 + (haze[ch] as f32 - c[ch] as f32) * t);
            let nearest = targets
                .iter()
                .copied()
                .min_by_key(|&j| {
                    let d = rgb[j];
                    (0..3)
                        .map(|ch| {
                            let e = d[ch] as f32 - want[ch];
                            (e * e) as u32
                        })
                        .sum::<u32>()
                })
                .unwrap_or(index);
            out[level * 256 + index] = nearest as u8;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dithering_spreads_between_neighbours() {
        let (mut low, mut high) = (0, 0);
        for y in 0..4 {
            for x in 0..4 {
                match shade_index(FAIRWAY, 5 * 16 + 8, x, y) - FAIRWAY {
                    5 => low += 1,
                    6 => high += 1,
                    other => panic!("{other}"),
                }
            }
        }
        assert_eq!((low, high), (8, 8));
        assert_eq!(shade_index(FAIRWAY, 999, 0, 0), FAIRWAY + 15);
        assert_eq!(shade_index(FAIRWAY, -5, 0, 0), FAIRWAY);
    }

    #[test]
    fn haze_level_zero_is_identity_for_ramps_and_full_haze_is_pale() {
        let p = Palette::new();
        for i in [FAIRWAY + 7, SAND + 3, ROOF + 12] {
            assert_eq!(p.fog[usize::from(i)], i);
        }
        let far = p.rgb[usize::from(p.fog[15 * 256 + usize::from(FOREST_FLOOR)])];
        let near = p.rgb[usize::from(FOREST_FLOOR)];
        assert!(
            far.iter().map(|&v| u32::from(v)).sum::<u32>()
                > near.iter().map(|&v| u32::from(v)).sum::<u32>() + 200
        );
    }

    #[test]
    fn water_animates_smoothly_and_loops() {
        let mut p = Palette::new();
        let entry = usize::from(WATER) + 5;
        let before = p.rgb[entry];
        p.animate_water(WATER_CYCLE * 0.25);
        assert_ne!(p.rgb[entry], before);
        p.animate_water(WATER_CYCLE);
        assert_eq!(p.rgb[entry], before);
        // A 60 ms step never jumps by more than a few levels.
        let mut last = p.rgb[entry];
        for k in 1..60 {
            p.animate_water(k as f32 * 0.06);
            let now = p.rgb[entry];
            let step = (0..3)
                .map(|c| (i16::from(now[c]) - i16::from(last[c])).abs())
                .max()
                .unwrap();
            assert!(step <= 4, "a {step}-level jump");
            last = now;
        }
    }
}
