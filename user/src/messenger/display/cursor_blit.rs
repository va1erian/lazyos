//! Drawing the antialiased cursor sprite (docs/cursor-plan.md): a source-over
//! blit of premultiplied ARGB, the format `libs/cursorgfx` renders.

use super::canvas::{Canvas, PixelLayout, Rect};

impl Canvas {
    /// Blend a premultiplied `0xAARRGGBB` sprite, `w` pixels per row, with
    /// its top-left at `(x, y)`, clipped to `clip` and the canvas.
    pub fn blit_premultiplied(&mut self, x: i32, y: i32, w: i32, pixels: &[u32], clip: Rect) {
        if w <= 0 {
            return;
        }
        let h = (pixels.len() / w as usize) as i32;
        // `visible` clips with saturating maths, so an extreme pointer
        // position yields an empty rectangle rather than an overflow.
        let (ox, oy) = (x, y);
        let r = self.visible(Rect::new(ox, oy, w, h), clip);
        let order = match self.layout {
            PixelLayout::Rgba => [16, 8, 0],
            PixelLayout::Bgra => [0, 8, 16],
        };
        for py in r.y..r.y + r.h {
            let first = ((py - oy) * w + (r.x - ox)) as usize;
            let src = &pixels[first..first + r.w as usize];
            let row = self.row_mut(r.x, py, r.w);
            for (dst, &p) in row.as_chunks_mut::<4>().0.iter_mut().zip(src) {
                let alpha = p >> 24;
                if alpha == 0 {
                    continue;
                }
                for (d, shift) in dst.iter_mut().zip(order) {
                    let s = (p >> shift) & 0xff;
                    *d = if alpha == 0xff {
                        s as u8
                    } else {
                        (s + (*d as u32 * (255 - alpha) + 127) / 255).min(255) as u8
                    };
                }
                dst[3] = 0xff;
            }
        }
    }
}
