//! Writes every shape at one size as a binary PPM contact sheet (grey
//! checkerboard behind the sprites) for looking at the rasteriser:
//! `cargo run -p cursorgfx --example dump -- 32 sheet.ppm`.

use cursorgfx::{render, Shape};
use std::io::Write;

fn main() {
    let mut args = std::env::args().skip(1);
    let px: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(32);
    let out = args.next().unwrap_or_else(|| "cursors.ppm".into());
    let cell = cursorgfx::max_extent(px);
    let width = cell * Shape::ALL.len() as u32;
    let mut rgb = vec![0u8; (width * cell * 3) as usize];
    for (n, shape) in Shape::ALL.iter().enumerate() {
        let sprite = render(*shape, px, 2);
        for y in 0..cell {
            for x in 0..cell {
                let back = if (x / 4 + y / 4) % 2 == 0 { 120 } else { 160 };
                let p = sprite.pixels[(y * sprite.w + x) as usize];
                let a = p >> 24;
                let mix = |c: u32| (c + back * (255 - a) / 255) as u8;
                let at = ((y * width + n as u32 * cell + x) * 3) as usize;
                rgb[at] = mix((p >> 16) & 0xff);
                rgb[at + 1] = mix((p >> 8) & 0xff);
                rgb[at + 2] = mix(p & 0xff);
            }
        }
    }
    let mut file = std::fs::File::create(out).unwrap();
    write!(file, "P6\n{width} {cell}\n255\n").unwrap();
    file.write_all(&rgb).unwrap();
}
