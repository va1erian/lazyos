//! The OpenRaster reader's limits on a hostile file: decoding work across
//! layers, pictures out of proportion to the canvas, and a zip whose entries
//! contradict themselves.

use super::zip::{ZipReader, ZipWriter};
use super::*;

/// An ORA of `canvas` whose `count` layers all name the one entry
/// `data/layer0.png`, a solid `picture`-sized PNG.
fn repeated(canvas: (u32, u32), picture: (u32, u32), count: usize) -> Vec<u8> {
    let mut xml = format!("<image w=\"{}\" h=\"{}\"><stack>", canvas.0, canvas.1);
    for _ in 0..count {
        xml.push_str("<layer name=\"l\" src=\"data/layer0.png\"/>");
    }
    xml.push_str("</stack></image>");
    let png = Bitmap::new(picture.0, picture.1, [9, 9, 9, 255])
        .encode_png()
        .unwrap();
    let mut zip = ZipWriter::new();
    zip.add("mimetype", b"image/openraster").unwrap();
    zip.add("stack.xml", xml.as_bytes()).unwrap();
    zip.add("data/layer0.png", &png).unwrap();
    zip.finish().unwrap()
}

#[test]
fn one_entry_named_by_many_layers_is_decoded_a_bounded_number_of_times() {
    // 1000 x 1000 is a million pixels; the cap is 128 million for the file.
    let few = repeated((512, 512), (1000, 1000), 40);
    assert!(open(&few).is_ok(), "40 layers are well inside the cap");
    let start = std::time::Instant::now();
    let many = repeated((512, 512), (1000, 1000), 200);
    let error = open(&many).expect_err("200 million pixels of decoding");
    assert!(error.contains("too large"), "{error}");
    assert!(start.elapsed().as_secs_f64() < 60.0);
}

#[test]
fn a_picture_far_larger_than_the_canvas_is_refused() {
    let error = open(&repeated((100, 100), (900, 900), 1)).expect_err("far larger");
    assert!(error.contains("far larger"), "{error}");
    // A layer a little over the canvas (content outside the page) is fine.
    assert!(open(&repeated((100, 100), (150, 150), 1)).is_ok());
}

#[test]
fn two_entries_of_one_name_are_refused() {
    let mut zip = ZipWriter::new();
    zip.add("mimetype", b"image/openraster").unwrap();
    zip.add("stack.xml", b"<image w=\"1\" h=\"1\"/>").unwrap();
    zip.add("stack.xml", b"<image w=\"2\" h=\"2\"/>").unwrap();
    let bytes = zip.finish().unwrap();
    let error = ZipReader::new(&bytes).err().expect("duplicate names");
    assert!(error.contains("two entries"), "{error}");
}

#[test]
fn a_local_header_that_names_another_entry_is_refused() {
    let mut zip = ZipWriter::new();
    zip.add("aaaa", b"one").unwrap();
    zip.add("bbbb", b"two").unwrap();
    let mut bytes = zip.finish().unwrap();
    // The first local header's name sits right after its 30 byte header.
    bytes[30..34].copy_from_slice(b"zzzz");
    let reader = ZipReader::new(&bytes).unwrap();
    assert!(reader.read("aaaa", 100).is_err());
    assert_eq!(reader.read("bbbb", 100).unwrap().unwrap(), b"two");
}
