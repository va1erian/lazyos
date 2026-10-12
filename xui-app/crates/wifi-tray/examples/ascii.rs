//! Prints every glyph as text, to look at the icon without a guest.
//!
//! `cargo run -p wifitray --example ascii`

use wifitray::Glyph;

fn main() {
    let glyphs = [
        Glyph::Bars(4),
        Glyph::Bars(2),
        Glyph::Bars(1),
        Glyph::Disconnected,
        Glyph::Connecting(1),
        Glyph::Unavailable,
    ];
    for glyph in glyphs {
        println!("{}", glyph.name());
        let mask = glyph.mask();
        for y in (0..mask.height).step_by(1) {
            let line: String = (0..mask.width)
                .map(|x| match mask.alpha(x, y) {
                    0..=20 => ' ',
                    21..=90 => '.',
                    91..=200 => 'o',
                    _ => '#',
                })
                .collect();
            println!("|{line}|");
        }
    }
}
