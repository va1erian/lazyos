use super::*;

const OLD_ARROW: [u8; 8] = [0x80, 0xC0, 0xA0, 0x90, 0x88, 0x84, 0xFC, 0xC0];

fn alpha(pixel: u32) -> u32 {
    pixel >> 24
}

fn checksum(sprite: &Sprite) -> u32 {
    sprite
        .pixels
        .iter()
        .fold(0x811c_9dc5u32, |h, &p| (h ^ p).wrapping_mul(0x0100_0193))
}

#[test]
fn every_shape_renders_in_bounds_at_every_size() {
    for shape in Shape::ALL {
        for size in SIZES {
            for scale in 1..=4 {
                let px = size * scale;
                let sprite = render(shape, px, 0);
                assert!(sprite.w <= max_extent(px) && sprite.h <= max_extent(px));
                assert_eq!(sprite.pixels.len(), (sprite.w * sprite.h) as usize);
                assert!(sprite.hot_x < sprite.w && sprite.hot_y < sprite.h);
                let opaque = sprite.pixels.iter().filter(|&&p| alpha(p) == 255).count();
                assert!(opaque > (px * px / 40) as usize, "{shape:?} {px}: {opaque}");
                // Premultiplied: no channel exceeds alpha.
                for &p in &sprite.pixels {
                    let a = alpha(p);
                    assert!(((p >> 16) & 0xff) <= a && ((p >> 8) & 0xff) <= a && (p & 0xff) <= a);
                }
            }
        }
    }
}

#[test]
fn edges_are_antialiased() {
    for shape in Shape::ALL {
        let sprite = render(shape, 32, 0);
        let partial = sprite
            .pixels
            .iter()
            .filter(|&&p| alpha(p) > 0 && alpha(p) < 255)
            .count();
        assert!(partial > 8, "{shape:?} has {partial} partial pixels");
    }
}

#[test]
fn the_arrow_keeps_the_old_silhouette() {
    // At 16 px a bitmap cell is 2x2 pixels; every old body pixel is covered.
    let sprite = render(Shape::Default, 16, 0);
    let m = margin(16);
    assert_eq!((sprite.hot_x, sprite.hot_y), (m, m));
    for (row, bits) in OLD_ARROW.iter().enumerate() {
        for col in 0..8 {
            if bits & (0x80 >> col) == 0 {
                continue;
            }
            let (x, y) = (m + col * 2, m + row as u32 * 2);
            let covered = alpha(sprite.pixels[(y * sprite.w + x) as usize]) > 0;
            assert!(covered, "cell {col},{row} lost");
        }
    }
}

#[test]
fn the_body_is_light_and_the_outline_dark() {
    let sprite = render(Shape::Default, 32, 0);
    let m = margin(32);
    // A pixel well inside the triangle is the light fill.
    let inner = sprite.pixels[((m + 8) * sprite.w + m + 3) as usize];
    assert_eq!(inner, 0xff_f0_f0_f0);
    // Just left of the tip's left edge is the dark outline.
    let outline = sprite.pixels[((m + 10) * sprite.w + m - 1) as usize];
    assert_eq!(outline & 0x00ff_ffff, 0);
    assert!(alpha(outline) > 0);
}

#[test]
fn output_is_deterministic_and_scales_with_size() {
    assert_eq!(render(Shape::Hand, 32, 0), render(Shape::Hand, 32, 0));
    assert!(render(Shape::Hand, 48, 0).w > render(Shape::Hand, 16, 0).w);
    assert_eq!(render(Shape::Hand, 32, 0), render(Shape::Hand, 32, 5));
}

#[test]
fn golden_checksums() {
    let golden = [
        (Shape::Default, GOLDEN[0]),
        (Shape::Busy, GOLDEN[1]),
        (Shape::Text, GOLDEN[2]),
        (Shape::Hand, GOLDEN[3]),
        (Shape::ResizeH, GOLDEN[4]),
        (Shape::ResizeV, GOLDEN[5]),
        (Shape::ResizeNwse, GOLDEN[6]),
        (Shape::ResizeNesw, GOLDEN[7]),
    ];
    for (shape, want) in golden {
        assert_eq!(checksum(&render(shape, 32, 0)), want, "{shape:?}");
    }
}

#[test]
fn resize_shapes_are_symmetric_where_they_should_be() {
    let h = render(Shape::ResizeH, 32, 0);
    let v = render(Shape::ResizeV, 32, 0);
    for y in 0..h.h {
        for x in 0..h.w {
            let (a, b) = (
                h.pixels[(y * h.w + x) as usize],
                v.pixels[(x * v.w + y) as usize],
            );
            assert!(alpha(a).abs_diff(alpha(b)) <= 32, "{x},{y}");
        }
    }
    // The two diagonals mirror each other.
    let a = render(Shape::ResizeNwse, 32, 0);
    let b = render(Shape::ResizeNesw, 32, 0);
    for y in 0..a.h {
        for x in 0..a.w {
            let mirrored = b.pixels[(y * b.w + (b.w - 1 - x)) as usize];
            let own = a.pixels[(y * a.w + x) as usize];
            assert!(alpha(own).abs_diff(alpha(mirrored)) <= 64);
        }
    }
}

#[test]
fn busy_frames_differ_and_wrap() {
    let frames: alloc::vec::Vec<_> = (0..BUSY_FRAMES)
        .map(|f| render(Shape::Busy, 32, f))
        .collect();
    for i in 0..frames.len() {
        for j in i + 1..frames.len() {
            assert_ne!(frames[i].pixels, frames[j].pixels, "{i} vs {j}");
        }
    }
    assert_eq!(render(Shape::Busy, 32, BUSY_FRAMES), frames[0]);
    assert!(Shape::Busy.animated() && !Shape::Hand.animated());
}

#[test]
fn wire_values_round_trip() {
    for shape in Shape::ALL {
        assert_eq!(Shape::from_wire(shape.to_wire()), Some(shape));
    }
    assert_eq!(Shape::from_wire(8), None);
    assert_eq!(Shape::from_wire(u32::MAX), None);
}

#[test]
fn extreme_sizes_are_clamped() {
    assert_eq!(render(Shape::Default, 0, 0).w, max_extent(0));
    assert_eq!(render(Shape::Default, 100_000, 0).w, max_extent(100_000));
}

#[test]
#[ignore]
fn print_goldens() {
    for shape in Shape::ALL {
        std::println!("{:#x},", checksum(&render(shape, 32, 0)));
    }
}

const GOLDEN: [u32; 8] = [
    0x4f56dca5, 0x50955230, 0xcc36db45, 0x9f9eb170, 0x38b9df65, 0xccd065b5, 0x5075c329, 0x53bf1c39,
];
