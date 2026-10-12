use super::zip::{ZipReader, ZipWriter};
use super::*;
use crate::model::{Layer, Model, WHITE};

/// A 5 x 4 bitmap of one colour with a single odd pixel, so flips and offsets show.
fn art(color: [u8; 4], dot: [u8; 4]) -> Bitmap {
    let mut b = Bitmap::new(5, 4, color);
    b.put(3, 2, dot);
    b
}

/// Three layers with awkward names, opacities and visibility; layer 1 is active.
pub(super) fn layered() -> Document {
    let mut bottom = Layer::new("Background", Bitmap::white(5, 4));
    bottom.pixels.put(0, 0, [10, 20, 30, 255]);
    let mut middle = Layer::new("A & <B> \"c\"", art([0, 0, 0, 0], [200, 100, 50, 255]));
    middle.opacity = 128;
    let mut top = Layer::new("Top", art([0, 0, 255, 90], [255, 0, 0, 255]));
    top.visible = false;
    Document::from_layers(vec![bottom, middle, top], 1).unwrap()
}

fn entry(zip: &[u8], name: &str) -> Vec<u8> {
    ZipReader::new(zip)
        .unwrap()
        .read(name, 1 << 24)
        .unwrap()
        .unwrap()
}

#[test]
fn an_ora_file_round_trips_layers_names_opacity_visibility_and_the_active_layer() {
    let doc = layered();
    let bytes = save(Format::OpenRaster, &doc).unwrap();
    let (format, back) = open(&bytes).unwrap();
    assert_eq!(format, Format::OpenRaster);
    assert_eq!(back, doc);
    assert_eq!(back.active(), 1);
    assert_eq!(back.layers()[1].name, "A & <B> \"c\"");
    assert_eq!(back.layers()[1].opacity, 128);
    assert!(!back.layers()[2].visible);
}

#[test]
fn every_opacity_survives_the_decimal_text() {
    for opacity in 0..=255u8 {
        let mut layer = Layer::new("L", Bitmap::white(1, 1));
        layer.opacity = opacity;
        let doc = Document::from_layers(vec![layer], 0).unwrap();
        let back = open(&save(Format::OpenRaster, &doc).unwrap()).unwrap().1;
        assert_eq!(back.layers()[0].opacity, opacity);
    }
}

#[test]
fn the_ora_container_follows_the_spec() {
    let bytes = save(Format::OpenRaster, &layered()).unwrap();
    // `mimetype` first, stored, no extra field: its text sits at byte 38.
    assert_eq!(&bytes[..4], b"PK\x03\x04");
    assert_eq!(u16::from_le_bytes([bytes[8], bytes[9]]), 0, "stored");
    assert_eq!(&bytes[30..38], b"mimetype");
    assert_eq!(&bytes[38..54], b"image/openraster");
    let zip = ZipReader::new(&bytes).unwrap();
    assert_eq!(zip.first_name(), Some("mimetype"));
    let names: Vec<_> = zip.names().collect();
    assert_eq!(
        names,
        [
            "mimetype",
            "stack.xml",
            "data/layer0.png",
            "data/layer1.png",
            "data/layer2.png",
            "mergedimage.png",
            "Thumbnails/thumbnail.png"
        ]
    );
    let xml = String::from_utf8(entry(&bytes, "stack.xml")).unwrap();
    assert!(xml.contains("<image w=\"5\" h=\"4\""), "{xml}");
    // The top layer is listed first, opacity is a 0..1 float, visibility a word.
    let top = xml.find("data/layer2.png").unwrap();
    let bottom = xml.find("data/layer0.png").unwrap();
    assert!(top < bottom, "{xml}");
    assert!(xml.contains("opacity=\"0.5020\""), "{xml}");
    assert!(xml.contains("visibility=\"hidden\""), "{xml}");
    assert!(
        xml.contains("name=\"A &amp; &lt;B&gt; &quot;c&quot;\""),
        "{xml}"
    );
    // The merged image is the composite; every PNG decodes.
    let merged = Bitmap::decode(&entry(&bytes, "mergedimage.png")).unwrap();
    assert_eq!(merged, layered().flatten());
    let layer1 = Bitmap::decode(&entry(&bytes, "data/layer1.png")).unwrap();
    assert_eq!(layer1, layered().layers()[1].pixels);
}

#[test]
fn the_thumbnail_is_at_most_256_square() {
    let doc = Document::new(1000, 400, [255, 0, 0, 255]).unwrap();
    let bytes = save(Format::OpenRaster, &doc).unwrap();
    let thumb = Bitmap::decode(&entry(&bytes, "Thumbnails/thumbnail.png")).unwrap();
    assert_eq!(thumb.size(), (256, 102));
    assert_eq!(thumb.get(5, 5), Some([255, 0, 0, 255]));
    let small = save(Format::OpenRaster, &layered()).unwrap();
    let thumb = Bitmap::decode(&entry(&small, "Thumbnails/thumbnail.png")).unwrap();
    assert_eq!(thumb.size(), (5, 4), "a small picture is not enlarged");
}

/// Builds an ORA by hand: `xml` as stack.xml and these named entries.
fn handmade(xml: &str, files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut zip = ZipWriter::new();
    zip.add("mimetype", b"image/openraster").unwrap();
    zip.add("stack.xml", xml.as_bytes()).unwrap();
    for (name, data) in files {
        zip.add(name, data).unwrap();
    }
    zip.finish().unwrap()
}

#[test]
fn offsets_nesting_and_foreign_attributes_are_handled() {
    // As GIMP or Krita write it: a layer cropped to its content with x/y, a
    // group at half opacity holding a layer, an unknown attribute, a comment.
    let dot = Bitmap::new(2, 2, [255, 0, 0, 255]).encode_png().unwrap();
    let full = Bitmap::new(6, 5, [0, 0, 255, 255]).encode_png().unwrap();
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
        <!-- written by hand -->
        <image version="0.0.5" w="6" h="5" xres="72" yres="72">
         <stack name="root" opacity="1" isolation="isolate">
          <layer name="dot" src="data/dot.png" x="1" y="2" opacity="1.0" visibility="visible" mystery="yes"/>
          <stack name="group" opacity="0.5" x="1" y="0">
           <layer name="inner" src="data/dot.png" x="0" y="0" selected="true"/>
          </stack>
          <layer name="" src="data/full.png"/>
         </stack>
        </image>"#;
    let bytes = handmade(xml, &[("data/dot.png", dot), ("data/full.png", full)]);
    let (_, doc) = open(&bytes).unwrap();
    assert_eq!(doc.size(), (6, 5));
    assert_eq!(doc.layers().len(), 3);
    // Bottom first: the unnamed full layer, then the group's, then the dot.
    assert_eq!(doc.layers()[0].name, "Layer 1");
    assert_eq!(doc.layers()[1].name, "inner");
    assert_eq!(
        doc.layers()[1].opacity,
        128,
        "the group's opacity multiplies in"
    );
    assert_eq!(doc.layers()[2].name, "dot");
    assert_eq!(doc.active(), 1, "selected=true");
    // The offsets place the 2 x 2 pictures on the canvas.
    assert_eq!(doc.layers()[2].pixels.get(1, 2), Some([255, 0, 0, 255]));
    assert_eq!(doc.layers()[2].pixels.get(2, 3), Some([255, 0, 0, 255]));
    assert_eq!(doc.layers()[2].pixels.get(0, 2), Some([0, 0, 0, 0]));
    assert_eq!(
        doc.layers()[1].pixels.get(1, 0),
        Some([255, 0, 0, 255]),
        "group x=1"
    );
    assert_eq!(doc.layers()[1].pixels.get(0, 0), Some([0, 0, 0, 0]));
}

#[test]
fn a_hidden_group_hides_what_is_in_it() {
    let png = Bitmap::white(2, 2).encode_png().unwrap();
    let xml = r#"<image w="2" h="2"><stack><stack visibility="hidden">
        <layer name="a" src="a.png"/></stack><layer name="b" src="a.png"/></stack></image>"#;
    let (_, doc) = open(&handmade(xml, &[("a.png", png)])).unwrap();
    assert!(!doc.layers()[1].visible && doc.layers()[0].visible);
    assert_eq!(doc.layers()[1].name, "a");
}

#[test]
fn bad_ora_files_are_refused_with_a_message() {
    let png = Bitmap::white(2, 2).encode_png().unwrap();
    let good = r#"<image w="2" h="2"><stack><layer name="a" src="a.png"/></stack></image>"#;
    assert!(open(&handmade(good, &[("a.png", png.clone())])).is_ok());
    type Case = (&'static str, String, Vec<(&'static str, Vec<u8>)>);
    let cases: Vec<Case> = vec![
        (
            "no size",
            r#"<image><stack><layer src="a.png"/></stack></image>"#.into(),
            vec![("a.png", png.clone())],
        ),
        (
            "zero size",
            r#"<image w="0" h="2"><stack><layer src="a.png"/></stack></image>"#.into(),
            vec![("a.png", png.clone())],
        ),
        (
            "huge side",
            r#"<image w="99999" h="2"><stack><layer src="a.png"/></stack></image>"#.into(),
            vec![("a.png", png.clone())],
        ),
        (
            "no layers",
            r#"<image w="2" h="2"><stack/></image>"#.into(),
            vec![],
        ),
        ("missing picture", good.into(), vec![]),
        (
            "no src",
            r#"<image w="2" h="2"><stack><layer name="a"/></stack></image>"#.into(),
            vec![("a.png", png.clone())],
        ),
        ("not a png", good.into(), vec![("a.png", b"hello".to_vec())]),
        (
            "cut xml",
            r#"<image w="2" h="2"><stack><layer src="a.png""#.into(),
            vec![("a.png", png.clone())],
        ),
    ];
    for (what, xml, files) in cases {
        let err = open(&handmade(&xml, &files)).unwrap_err();
        assert!(!err.is_empty(), "{what}");
    }
    // Wrong or missing mimetype, missing stack.xml.
    let mut zip = ZipWriter::new();
    zip.add("mimetype", b"text/plain").unwrap();
    zip.add("stack.xml", good.as_bytes()).unwrap();
    assert!(
        open(&zip.finish().unwrap())
            .unwrap_err()
            .contains("OpenRaster")
    );
    let mut zip = ZipWriter::new();
    zip.add("mimetype", b"image/openraster").unwrap();
    assert!(
        open(&zip.finish().unwrap())
            .unwrap_err()
            .contains("stack.xml")
    );
    // An ordinary zip is not a picture.
    let mut zip = ZipWriter::new();
    zip.add("readme.txt", b"hi").unwrap();
    assert!(open(&zip.finish().unwrap()).is_err());
}

#[test]
fn a_memory_bomb_is_refused_before_anything_is_allocated() {
    // 100 layers of 16384 x 16384 claimed by a few hundred bytes.
    let mut xml = String::from(r#"<image w="16384" h="16384"><stack>"#);
    for _ in 0..100 {
        xml.push_str(r#"<layer src="a.png"/>"#);
    }
    xml.push_str("</stack></image>");
    let started = std::time::Instant::now();
    let err = open(&handmade(&xml, &[])).unwrap_err();
    assert!(err.contains("memory"), "{err}");
    assert!(started.elapsed().as_secs() < 2);
    // Too many layers for the cap, even on a small canvas.
    let mut many = String::from(r#"<image w="1" h="1"><stack>"#);
    for _ in 0..=ora::MAX_LAYERS {
        many.push_str(r#"<layer src="a.png"/>"#);
    }
    many.push_str("</stack></image>");
    assert!(open(&handmade(&many, &[])).unwrap_err().contains("layers"));
    // Deep nesting is refused, not recursed into.
    let deep = format!(
        r#"<image w="1" h="1">{}<layer src="a.png"/>{}</image>"#,
        "<stack>".repeat(100),
        "</stack>".repeat(100)
    );
    assert!(open(&handmade(&deep, &[])).unwrap_err().contains("nested"));
}

/// A PNG whose header claims `w` x `h` and nothing else.
fn png_claiming(w: u32, h: u32) -> Vec<u8> {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
    b
}

#[test]
fn a_picture_that_claims_to_be_huge_is_refused_before_decoding() {
    assert_eq!(
        sniff_size(&png_claiming(30000, 20000)),
        Some((30000, 20000))
    );
    let err = open(&png_claiming(30000, 20000)).unwrap_err();
    assert!(err.contains("too large"), "{err}");
    let xml = r#"<image w="2" h="2"><stack><layer src="a.png"/></stack></image>"#;
    let err = open(&handmade(xml, &[("a.png", png_claiming(16384, 16384))])).unwrap_err();
    assert!(err.contains("too large"), "{err}");
}

#[test]
fn jpeg_headers_give_their_size() {
    // SOI, APP0 (len 4), SOF0: precision 8, height 16, width 32.
    let jpeg = [
        0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x10,
        0x00, 0x20, 0x01, 0x01, 0x11, 0x00,
    ];
    assert_eq!(sniff_size(&jpeg), Some((32, 16)));
    assert_eq!(Format::detect(&jpeg), Some(Format::Jpeg));
    assert_eq!(sniff_size(&jpeg[..12]), None);
    assert_eq!(sniff_size(&[0xFF, 0xD8]), None);
    assert_eq!(sniff_size(b"junk"), None);
    // A JPEG with garbage after the header is refused, not panicked on.
    assert!(open(&jpeg).is_err());
}

fn clean_file(format: Format, doc: &Document) -> Document {
    open(&save(format, doc).unwrap()).unwrap().1
}

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

#[test]
fn the_dispatcher_opens_by_bytes_and_saves_by_format() {
    let doc = Document::from_bitmap(art([5, 6, 7, 255], [250, 251, 252, 255])).unwrap();
    for format in [Format::Png, Format::Bmp, Format::OpenRaster] {
        let bytes = save(format, &doc).unwrap();
        assert_eq!(Format::detect(&bytes), Some(format), "{format:?}");
        let (found, back) = open(&bytes).unwrap();
        assert_eq!(found, format);
        assert_eq!(back.flatten(), doc.flatten(), "{format:?}");
    }
    assert!(save(Format::Jpeg, &doc).is_err());
    assert!(open(b"").is_err());
    assert!(open(b"hello world").is_err());
    assert!(open(b"\x89PNG\r\n\x1a\nnope").is_err());
}

#[test]
fn extensions_pick_the_save_format() {
    use std::path::Path;
    for (path, want) in [
        ("a.ora", Some(Format::OpenRaster)),
        ("dir/A.PNG", Some(Format::Png)),
        ("x.bmp", Some(Format::Bmp)),
        ("x.JPG", Some(Format::Jpeg)),
        ("x.jpeg", Some(Format::Jpeg)),
        ("x.txt", None),
        ("noext", None),
        ("archive.ora.bak", None),
    ] {
        assert_eq!(Format::from_path(Path::new(path)), want, "{path}");
    }
    assert!(Format::OpenRaster.keeps_layers() && !Format::Png.keeps_layers());
    assert!(Format::Png.can_save() && !Format::Jpeg.can_save());
    assert_eq!(Format::OpenRaster.mime(), "image/openraster");
}

#[test]
fn only_multi_layer_documents_need_flattening_for_flat_formats() {
    let one = Document::new(4, 4, WHITE).unwrap();
    let many = layered();
    for format in [Format::Png, Format::Bmp, Format::Jpeg] {
        assert!(!needs_flatten(format, &one));
        assert!(needs_flatten(format, &many));
    }
    assert!(!needs_flatten(Format::OpenRaster, &many));
}

#[test]
fn saving_flat_flattens_a_copy_and_leaves_the_document_alone() {
    let mut model = Model::new(5, 4);
    model.load_document(layered()).unwrap();
    let before = model.document().clone();
    let revision = model.revision();
    for format in [Format::Png, Format::Bmp] {
        assert!(model.needs_flatten(format));
        let bytes = model.save_bytes(format).unwrap();
        // The file is the composite, one picture.
        let back = open(&bytes).unwrap().1;
        assert_eq!(back.layers().len(), 1);
        assert_eq!(back.flatten(), before.flatten(), "{format:?}");
    }
    assert_eq!(
        model.document(),
        &before,
        "the document still has its layers"
    );
    assert_eq!(model.revision(), revision);
    assert_eq!(model.history().undo_len(), 0);
    // The clean round trip through every format keeps one-layer content.
    assert_eq!(clean_file(Format::OpenRaster, &before), before);
}

#[test]
fn the_model_opens_bytes_and_fails_without_changing_anything() {
    let mut model = Model::new(8, 8);
    model.begin(1, 1, crate::model::Side::Primary);
    model.end();
    let drawn = model.document().clone();
    assert!(model.open_bytes(b"not a picture").is_err());
    assert_eq!(model.document(), &drawn, "a failed open changes nothing");
    assert_eq!(model.history().undo_len(), 1);
    let bytes = save(Format::OpenRaster, &layered()).unwrap();
    assert_eq!(model.open_bytes(&bytes).unwrap(), Format::OpenRaster);
    assert_eq!(model.document(), &layered());
    assert_eq!(
        model.history().undo_len(),
        0,
        "a new document starts a new history"
    );
}

/// Written by `tests/fixtures/make_foreign_ora.py` with Python's zipfile:
/// deflated entries, cropped layers with offsets, a hidden group.
pub(super) const FOREIGN: &[u8] = include_bytes!("../../../tests/fixtures/foreign.ora");

#[test]
fn reads_a_file_another_zip_writer_made() {
    assert_eq!(Format::detect(FOREIGN), Some(Format::OpenRaster));
    let (_, doc) = open(FOREIGN).unwrap();
    assert_eq!(doc.size(), (6, 4));
    let names: Vec<_> = doc.layers().iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Paper", "Inside", "Spot"]);
    assert_eq!(doc.active(), 2);
    assert_eq!(doc.layers()[0].pixels.get(5, 3), Some([240, 240, 200, 255]));
    assert!(!doc.layers()[1].visible, "inherited from the hidden group");
    assert_eq!(doc.layers()[2].opacity, 191);
    let spot = &doc.layers()[2].pixels;
    assert_eq!(spot.get(2, 1), Some([255, 0, 0, 255]));
    assert_eq!(spot.get(3, 2), Some([255, 0, 0, 255]));
    assert_eq!(spot.get(1, 1), Some([0, 0, 0, 0]));
    assert_eq!(spot.get(4, 2), Some([0, 0, 0, 0]));
    // And it survives our own writer.
    let again = open(&save(Format::OpenRaster, &doc).unwrap()).unwrap().1;
    assert_eq!(again, doc);
}
