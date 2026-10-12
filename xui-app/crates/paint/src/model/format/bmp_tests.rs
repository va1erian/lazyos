//! BMP and flat-format tests, split from `tests.rs` to keep files short.

use super::*;

#[test]
fn bmp_round_trips_opaque_as_24_bit_and_alpha_as_32_bit() {
    for width in 1..=9u32 {
        let mut b = Bitmap::new(width, 3, [1, 2, 3, 255]);
        b.put(0, 0, [250, 0, 7, 255]);
        b.put(width as i32 - 1, 2, [9, 99, 199, 255]);
        let bytes = bmp::write(&b).unwrap();
        assert_eq!(u16::from_le_bytes([bytes[28], bytes[29]]), 24);
        let stride = (width as usize * 3).div_ceil(4) * 4;
        assert_eq!(bytes.len(), 14 + 40 + stride * 3, "width {width}");
        assert_eq!(bmp::read(&bytes).unwrap(), b, "width {width}");
    }
    let mut b = Bitmap::new(5, 3, [10, 20, 30, 128]);
    b.put(1, 1, [255, 0, 0, 0]);
    b.put(2, 2, [0, 255, 0, 255]);
    let bytes = bmp::write(&b).unwrap();
    assert_eq!(u16::from_le_bytes([bytes[28], bytes[29]]), 32);
    assert_eq!(
        u32::from_le_bytes(bytes[14..18].try_into().unwrap()),
        108,
        "V4 header"
    );
    let back = bmp::read(&bytes).unwrap();
    // Straight alpha: the colour of a half-transparent pixel is kept; a fully
    // transparent one keeps its colour too.
    assert_eq!(back, b);
}

/// A BMP header (file + info) for hand-built files.
fn bmp_header(w: i32, h: i32, bpp: u16, compression: u32, colors: u32, data_at: u32) -> Vec<u8> {
    let mut v = b"BM".to_vec();
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&data_at.to_le_bytes());
    v.extend_from_slice(&40u32.to_le_bytes());
    v.extend_from_slice(&w.to_le_bytes());
    v.extend_from_slice(&h.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&bpp.to_le_bytes());
    v.extend_from_slice(&compression.to_le_bytes());
    v.extend_from_slice(&[0; 12]);
    v.extend_from_slice(&colors.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v
}

#[test]
fn hand_built_bmp_files_read_as_the_spec_says() {
    // 2 x 2, 24 bit, bottom-up: the first row in the file is the bottom row.
    // Pixels are B, G, R; rows pad to 8 bytes.
    let mut f = bmp_header(2, 2, 24, 0, 0, 54);
    f.extend_from_slice(&[1, 2, 3, 4, 5, 6, 0, 0]); // bottom: (R3 G2 B1), (R6 G5 B4)
    f.extend_from_slice(&[7, 8, 9, 10, 11, 12, 0, 0]); // top
    let b = bmp::read(&f).unwrap();
    assert_eq!(b.get(0, 1), Some([3, 2, 1, 255]));
    assert_eq!(b.get(1, 1), Some([6, 5, 4, 255]));
    assert_eq!(b.get(0, 0), Some([9, 8, 7, 255]));
    assert_eq!(b.get(1, 0), Some([12, 11, 10, 255]));
    // The same data with a negative height is top-down.
    let mut g = bmp_header(2, -2, 24, 0, 0, 54);
    g.extend_from_slice(&f[54..]);
    let t = bmp::read(&g).unwrap();
    assert_eq!(t.get(0, 0), Some([3, 2, 1, 255]));
    assert_eq!(t.get(1, 1), Some([12, 11, 10, 255]));
    // 32 bit BI_RGB with every alpha byte 0 is opaque (the byte is unused).
    let mut h = bmp_header(2, 1, 32, 0, 0, 54);
    h.extend_from_slice(&[1, 2, 3, 0, 4, 5, 6, 0]);
    assert_eq!(bmp::read(&h).unwrap().get(1, 0), Some([6, 5, 4, 255]));
    // ... but a real alpha channel is used.
    let mut h = bmp_header(2, 1, 32, 0, 0, 54);
    h.extend_from_slice(&[1, 2, 3, 77, 4, 5, 6, 0]);
    assert_eq!(bmp::read(&h).unwrap().get(0, 0), Some([3, 2, 1, 77]));
    // 8 bit with a 2-entry palette (B, G, R, 0), indices 1 0 1 | pad.
    let mut p = bmp_header(3, 1, 8, 0, 2, 62);
    p.extend_from_slice(&[0, 0, 255, 0, 255, 0, 0, 0]); // 0 = red, 1 = blue
    p.extend_from_slice(&[1, 0, 1, 0]);
    let b = bmp::read(&p).unwrap();
    assert_eq!(b.get(0, 0), Some([0, 0, 255, 255]));
    assert_eq!(b.get(1, 0), Some([255, 0, 0, 255]));
    // 1 bit, 10 pixels: 1010101010 then padding to 4 bytes.
    let mut o = bmp_header(10, 1, 1, 0, 2, 62);
    o.extend_from_slice(&[0, 0, 0, 0, 255, 255, 255, 0]);
    o.extend_from_slice(&[0b1010_1010, 0b1000_0000, 0, 0]);
    let b = bmp::read(&o).unwrap();
    assert_eq!(b.get(0, 0), Some([255, 255, 255, 255]));
    assert_eq!(b.get(1, 0), Some([0, 0, 0, 255]));
    assert_eq!(b.get(8, 0), Some([255, 255, 255, 255]));
    assert_eq!(b.get(9, 0), Some([0, 0, 0, 255]));
    // 4 bit: high nibble first.
    let mut q = bmp_header(3, 1, 4, 0, 2, 62);
    q.extend_from_slice(&[0, 0, 0, 0, 255, 255, 255, 0]);
    q.extend_from_slice(&[0x10, 0x10, 0, 0]);
    let b = bmp::read(&q).unwrap();
    assert_eq!(b.get(0, 0), Some([255, 255, 255, 255]));
    assert_eq!(b.get(1, 0), Some([0, 0, 0, 255]));
    assert_eq!(b.get(2, 0), Some([255, 255, 255, 255]));
}

#[test]
fn bmp_bitfields_with_other_channel_orders() {
    // Masks RGBA = 0xFF000000, 0x00FF0000, 0x0000FF00, 0x000000FF.
    let mut f = bmp_header(1, 1, 32, 3, 0, 66); // 54 + three 4-byte masks
    for mask in [0xFF00_0000u32, 0x00FF_0000, 0x0000_FF00] {
        f.extend_from_slice(&mask.to_le_bytes());
    }
    f.extend_from_slice(&0x1122_3300u32.to_le_bytes());
    // No alpha mask in a 40 byte header: opaque.
    assert_eq!(
        bmp::read(&f).unwrap().get(0, 0),
        Some([0x11, 0x22, 0x33, 255])
    );
}

#[test]
fn bad_bmp_files_are_refused() {
    let mut f = bmp_header(2, 2, 24, 0, 0, 54);
    f.extend_from_slice(&[0; 16]);
    assert!(bmp::read(&f).is_ok());
    for (what, file) in [
        ("empty", Vec::new()),
        ("magic", b"BX".to_vec()),
        ("truncated data", f[..60].to_vec()),
        ("truncated header", f[..20].to_vec()),
        ("zero width", bmp_header(0, 2, 24, 0, 0, 54)),
        ("negative width", bmp_header(-2, 2, 24, 0, 0, 54)),
        ("zero height", bmp_header(2, 0, 24, 0, 0, 54)),
        ("RLE", bmp_header(2, 2, 8, 1, 0, 54)),
        ("16 bit", bmp_header(2, 2, 16, 0, 0, 54)),
        ("huge claim", bmp_header(16384, 16384, 24, 0, 0, 54)),
        ("beyond the cap", bmp_header(16385, 1, 24, 0, 0, 54)),
        ("data offset past the end", {
            let mut g = bmp_header(2, 2, 24, 0, 0, 0xFFFF_FFF0);
            g.extend_from_slice(&[0; 16]);
            g
        }),
    ] {
        assert!(bmp::read(&file).is_err(), "{what}");
    }
    let mut wide = bmp_header(i32::MIN, i32::MIN, 24, 0, 0, 54);
    wide.extend_from_slice(&[0; 16]);
    assert!(bmp::read(&wide).is_err());
}
