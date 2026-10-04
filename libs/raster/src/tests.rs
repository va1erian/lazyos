use alloc::vec;
use alloc::vec::Vec;

use crate::{decode, ColorSpace, DecodeError, EncodeError, Header, PageEncoder, HEADER_LEN, SYNC};

fn a4(color: ColorSpace) -> Header {
    Header {
        width: 2480,
        height: 3508,
        dpi: 300,
        color,
        media: "iso_a4_210x297mm".into(),
        quality: 4,
        total_pages: 2,
    }
}

fn be(h: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([h[at], h[at + 1], h[at + 2], h[at + 3]])
}

#[test]
fn the_header_says_what_ippeveprinter_reads() {
    let h = a4(ColorSpace::Srgb8).to_bytes();
    assert_eq!(h.len(), HEADER_LEN);
    assert_eq!(&h[..10], b"PwgRaster\0");
    assert_eq!((be(&h, 276), be(&h, 280)), (300, 300));
    // A4 in points: 595 x 842.
    assert_eq!((be(&h, 352), be(&h, 356)), (595, 842));
    assert_eq!((be(&h, 372), be(&h, 376)), (2480, 3508));
    assert_eq!((be(&h, 384), be(&h, 388), be(&h, 392)), (8, 24, 2480 * 3));
    assert_eq!((be(&h, 400), be(&h, 420)), (19, 3));
    assert_eq!(be(&h, 452), 2);
    assert_eq!(&h[1732..1749], b"iso_a4_210x297mm\0");
    let g = a4(ColorSpace::Sgray8).to_bytes();
    assert_eq!(
        (be(&g, 388), be(&g, 392), be(&g, 400), be(&g, 420)),
        (8, 2480, 18, 1)
    );
    let back = Header::from_bytes(&g.try_into().unwrap()).unwrap();
    assert_eq!(back, a4(ColorSpace::Sgray8));
}

#[test]
fn a_blank_page_is_a_few_bytes() {
    let header = a4(ColorSpace::Srgb8);
    let mut encoder = PageEncoder::new(header.clone());
    let white = vec![255u8; 2480 * 4];
    for _ in 0..3508 {
        encoder.push_rgba(&white).unwrap();
    }
    let body = encoder.finish().unwrap();
    // 3508 rows = 13 full repeats of 256 + 180; each line is 2480 white
    // pixels = 19 runs of 128 + one of 48, 4 bytes per run.
    assert_eq!(body.len(), HEADER_LEN + 14 * (1 + 20 * 4));
    let mut stream = SYNC.to_vec();
    stream.extend(body);
    let pages = decode(&stream, usize::MAX).unwrap();
    assert_eq!(pages[0].pixels.len(), 2480 * 3508 * 3);
    assert!(pages[0].pixels.iter().all(|&b| b == 255));
}

#[test]
fn runs_and_literals_are_coded_as_the_spec_says() {
    let header = Header {
        width: 6,
        height: 2,
        color: ColorSpace::Sgray8,
        ..a4(ColorSpace::Sgray8)
    };
    let mut encoder = PageEncoder::new(header);
    encoder.push_row(&[1, 2, 3, 3, 3, 9]).unwrap();
    encoder.push_row(&[7, 7, 7, 7, 7, 7]).unwrap();
    let body = encoder.finish().unwrap();
    assert_eq!(
        &body[HEADER_LEN..],
        &[
            0, // one line
            255, 1, 2, // two literals: 257 - 2
            2, 3, // three 3s
            0, 9, // a lone pixel is a run of one
            0, // one line
            5, 7, // six 7s
        ]
    );
}

#[test]
fn rgba_rows_become_grey_or_rgb() {
    let header = Header {
        width: 3,
        height: 1,
        ..a4(ColorSpace::Sgray8)
    };
    let rgba = [255, 255, 255, 255, 0, 0, 0, 255, 255, 0, 0, 255];
    let mut grey = PageEncoder::new(header.clone());
    grey.push_rgba(&rgba).unwrap();
    let mut stream = SYNC.to_vec();
    stream.extend(grey.finish().unwrap());
    assert_eq!(decode(&stream, 64).unwrap()[0].pixels, [255, 0, 54]);

    let mut rgb = PageEncoder::new(Header {
        color: ColorSpace::Srgb8,
        ..header
    });
    rgb.push_rgba(&rgba).unwrap();
    let mut stream = SYNC.to_vec();
    stream.extend(rgb.finish().unwrap());
    assert_eq!(
        decode(&stream, 64).unwrap()[0].pixels,
        [255, 255, 255, 0, 0, 0, 255, 0, 0]
    );
}

#[test]
fn output_streams_while_rows_arrive() {
    let header = Header {
        width: 64,
        height: 600,
        ..a4(ColorSpace::Sgray8)
    };
    let mut encoder = PageEncoder::new(header.clone());
    let mut stream = SYNC.to_vec();
    let mut want = Vec::new();
    for y in 0..600u32 {
        let row: Vec<u8> = (0..64u32).map(|x| ((x * 7 + y / 3) % 256) as u8).collect();
        want.extend_from_slice(&row);
        encoder.push_row(&row).unwrap();
        stream.extend(encoder.take_output());
    }
    stream.extend(encoder.finish().unwrap());
    assert_eq!(decode(&stream, usize::MAX).unwrap()[0].pixels, want);
}

#[test]
fn wrong_rows_are_refused() {
    let header = Header {
        width: 4,
        height: 1,
        ..a4(ColorSpace::Srgb8)
    };
    let mut encoder = PageEncoder::new(header.clone());
    assert_eq!(encoder.push_row(&[0; 4]), Err(EncodeError::RowLength));
    assert_eq!(encoder.push_rgba(&[0; 12]), Err(EncodeError::RowLength));
    encoder.push_rgba(&[0; 16]).unwrap();
    assert_eq!(encoder.push_rgba(&[0; 16]), Err(EncodeError::TooManyRows));
    let short = PageEncoder::new(Header {
        height: 2,
        ..header
    });
    assert_eq!(
        short.finish(),
        Err(EncodeError::MissingRows {
            given: 0,
            height: 2
        })
    );
}

#[test]
fn bad_streams_are_refused() {
    assert_eq!(decode(b"RaS3", 100), Err(DecodeError::BadSync));
    let mut stream = SYNC.to_vec();
    stream.extend(a4(ColorSpace::Srgb8).to_bytes());
    assert_eq!(decode(&stream, usize::MAX), Err(DecodeError::Truncated));
    assert_eq!(decode(&stream, 1000), Err(DecodeError::TooLarge));
    // A line repeat past the page's last row.
    let small = Header {
        width: 1,
        height: 2,
        ..a4(ColorSpace::Sgray8)
    };
    let mut over = SYNC.to_vec();
    over.extend(small.to_bytes());
    over.extend([2, 0, 9]);
    assert_eq!(decode(&over, 100), Err(DecodeError::Overrun));
    // A run past the end of its line.
    let mut wide = SYNC.to_vec();
    wide.extend(small.to_bytes());
    wide.extend([1, 1, 9]);
    assert_eq!(decode(&wide, 100), Err(DecodeError::Overrun));
}
