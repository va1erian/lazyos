//! A mouse cursor sprite drawn as a framebuffer overlay.

use crate::gfx::Color;
use crate::surface::Surface;

/// Sprite bitmap: 'X' = white fill, '.' = black outline, ' ' = transparent.
const CURSOR: &[&str] = &[
    "X           ",
    "XX          ",
    "X.X         ",
    "X..X        ",
    "X...X       ",
    "X....X      ",
    "X.....X     ",
    "X......X    ",
    "X.......X   ",
    "X........X  ",
    "X.....XXXXX ",
    "X..X..X     ",
    "X.X.X..X    ",
    "XX..X..X    ",
    "X    X..X   ",
    "     XX     ",
];

/// Sprite width and height in pixels.
pub const WIDTH: i32 = 12;
pub const HEIGHT: i32 = 16;

const WHITE: Color = Color::rgb(255, 255, 255);
const BLACK: Color = Color::rgb(20, 20, 30);

/// Draw the cursor with its tip at `(x, y)`.
pub fn draw(surface: &mut impl Surface, x: i32, y: i32) {
    for (row, line) in CURSOR.iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            let px = x + col as i32;
            let py = y + row as i32;
            if px < 0 || py < 0 {
                continue;
            }
            match ch {
                'X' => surface.set(px as usize, py as usize, WHITE),
                '.' => surface.set(px as usize, py as usize, BLACK),
                _ => {}
            }
        }
    }
}
