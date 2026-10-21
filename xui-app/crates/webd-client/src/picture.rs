//! The persistent picture: the last whole frame webd painted, copied out of the slot so the slot can
//! go back to webd at once.

/// The colour a picture is mostly made of, the way `webkit-views` tells its pages apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Colour {
    Red,
    Green,
    Blue,
    White,
    /// None of them covers half the picture.
    Mixed,
}

impl Colour {
    /// The word the evidence lines use.
    pub fn name(self) -> &'static str {
        match self {
            Colour::Red => "red",
            Colour::Green => "green",
            Colour::Blue => "blue",
            Colour::White => "white",
            Colour::Mixed => "none",
        }
    }
}

/// An RGBA8 picture, `width * 4` bytes per row, no padding.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Picture {
    width: u32,
    height: u32,
    /// The frame number it came from (0: nothing painted yet).
    seq: u64,
    rgba: Vec<u8>,
}

impl Picture {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The number of the frame on show; 0 before the first.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn is_empty(&self) -> bool {
        self.seq == 0
    }

    /// The pixels, RGBA8 in rows.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Make room for a `width` x `height` frame and return its bytes to fill.
    pub(crate) fn begin(&mut self, width: u32, height: u32, seq: u64) -> &mut [u8] {
        self.width = width;
        self.height = height;
        self.seq = seq;
        self.rgba.resize(width as usize * height as usize * 4, 0);
        &mut self.rgba
    }

    /// The colour most of the picture is, counting a pixel only when it is clearly that colour (a
    /// page of text on white is white).
    pub fn dominant(&self) -> Colour {
        let mut counts = [0usize; 4];
        for pixel in self.rgba.chunks_exact(4) {
            let (r, g, b) = (pixel[0], pixel[1], pixel[2]);
            if r > 200 && g < 80 && b < 80 {
                counts[0] += 1;
            } else if g > 100 && r < 80 && b < 80 {
                counts[1] += 1;
            } else if b > 200 && r < 80 && g < 80 {
                counts[2] += 1;
            } else if r > 240 && g > 240 && b > 240 {
                counts[3] += 1;
            }
        }
        let total = self.rgba.len() / 4;
        [Colour::Red, Colour::Green, Colour::Blue, Colour::White]
            .into_iter()
            .zip(counts)
            .find(|(_, count)| count * 2 > total)
            .map_or(Colour::Mixed, |(colour, _)| colour)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(r: u8, g: u8, b: u8) -> Picture {
        let mut picture = Picture::default();
        for pixel in picture.begin(4, 4, 1).chunks_exact_mut(4) {
            pixel.copy_from_slice(&[r, g, b, 255]);
        }
        picture
    }

    #[test]
    fn the_dominant_colour_is_named() {
        assert_eq!(filled(255, 0, 0).dominant(), Colour::Red);
        assert_eq!(filled(0, 128, 0).dominant(), Colour::Green);
        assert_eq!(filled(10, 10, 255).dominant(), Colour::Blue);
        assert_eq!(filled(255, 255, 255).dominant(), Colour::White);
        assert_eq!(filled(120, 120, 120).dominant(), Colour::Mixed);
        assert_eq!(Picture::default().dominant(), Colour::Mixed);
    }

    #[test]
    fn half_is_not_enough() {
        let mut picture = filled(255, 255, 255);
        let bytes = picture.begin(4, 4, 2);
        for pixel in bytes.chunks_exact_mut(4).take(8) {
            pixel.copy_from_slice(&[0, 0, 255, 255]);
        }
        assert_eq!(picture.dominant(), Colour::Mixed);
    }
}
