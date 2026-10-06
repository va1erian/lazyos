//! `lazypdf` against the sample documents `tools/pdf/make_sample.py` writes.

use std::sync::Arc;

use lazypdf::{Document, OpenError, Renderer};

fn testdata(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn sample() -> Document {
    Document::open(testdata("sample.pdf"), "").expect("the sample opens")
}

#[test]
fn the_sample_has_six_pages_with_their_displayed_sizes() {
    let doc = sample();
    assert_eq!(doc.page_count(), 6);
    let a4 = doc.page_size(0).unwrap();
    assert!(
        (a4.width - 595.0).abs() < 1.0 && (a4.height - 842.0).abs() < 1.0,
        "{a4:?}"
    );
    // Page 4 is A4 with /Rotate 90: displayed in landscape.
    let rotated = doc.page_size(3).unwrap();
    assert!(rotated.width > rotated.height, "{rotated:?}");
    // Page 5 is landscape by its MediaBox.
    let landscape = doc.page_size(4).unwrap();
    assert!(landscape.width > landscape.height, "{landscape:?}");
    assert_eq!(doc.page_size(6), None);
}

#[test]
fn metadata_is_decoded() {
    let doc = sample();
    let info = doc.info();
    assert_eq!(info.title.as_deref(), Some("LazyOS PDF Viewer sample"));
    assert_eq!(info.author.as_deref(), Some("LazyOS"));
    assert_eq!(doc.version(), "1.7");
}

#[test]
fn encrypted_files_need_their_password() {
    for name in ["sample-aes256.pdf", "sample-rc4.pdf"] {
        assert_eq!(
            Document::open(testdata(name), "").err(),
            Some(OpenError::Password),
            "{name}"
        );
        assert_eq!(
            Document::open(testdata(name), "wrong").err(),
            Some(OpenError::Password),
            "{name}"
        );
        let doc = Document::open(testdata(name), "lazyos").expect(name);
        assert_eq!(doc.page_count(), 6, "{name}");
        let text = Renderer::new(&doc).page_text(1).unwrap().plain();
        assert!(text.contains("lighthouse"), "{name}: {text}");
    }
}

#[test]
fn garbage_is_refused_not_a_panic() {
    for bytes in [
        Vec::new(),
        b"not a pdf at all".to_vec(),
        b"%PDF-1.7\n".to_vec(),
        b"%PDF-1.4\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF".to_vec(),
    ] {
        assert!(Document::open(bytes.clone(), "").is_err(), "{bytes:?}");
    }
    // A truncated real file either opens (the xref rebuild) or is refused.
    let mut cut = testdata("sample.pdf");
    cut.truncate(cut.len() / 2);
    if let Ok(doc) = Document::open(cut, "") {
        let r = Renderer::new(&doc);
        for i in 0..doc.page_count() {
            let _ = r.render_page(i, 0.25);
        }
    }
}

#[test]
fn a_page_is_white_with_ink_on_it() {
    let doc = sample();
    let r = Renderer::new(&doc);
    let tile = r.render_page(0, 0.5).unwrap();
    assert_eq!((tile.width, tile.height), r.page_pixels(0, 0.5).unwrap());
    let px = |x: u32, y: u32| {
        let i = ((y * tile.width + x) * 4) as usize;
        [
            tile.rgba[i],
            tile.rgba[i + 1],
            tile.rgba[i + 2],
            tile.rgba[i + 3],
        ]
    };
    assert_eq!(px(1, 1), [255, 255, 255, 255], "the margin is white");
    let dark = tile
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] < 100 && p[1] < 100 && p[2] < 100)
        .count();
    assert!(dark > 500, "the title page has text: {dark} dark pixels");
}

#[test]
fn tiles_match_the_same_region_of_a_whole_page() {
    let doc = sample();
    let r = Renderer::new(&doc);
    let scale = 96.0 / 72.0;
    let whole = r.render_page(2, scale).unwrap();
    let (x, y, w, h) = (100, 150, 256, 256);
    let tile = r.render_tile(2, scale, x, y, w, h).unwrap();
    let mut worst = 0u8;
    for row in 0..h {
        for col in 0..w {
            let t = ((row * w + col) * 4) as usize;
            let p = (((y + row) * whole.width + x + col) * 4) as usize;
            for c in 0..3 {
                worst = worst.max(tile.rgba[t + c].abs_diff(whole.rgba[p + c]));
            }
        }
    }
    // Anti-aliasing may differ by a level or two at tile edges, never more.
    assert!(worst <= 8, "a tile differs from the page by {worst}");
}

#[test]
fn bad_tile_requests_are_none() {
    let doc = sample();
    let r = Renderer::new(&doc);
    assert!(r.render_tile(0, 1.0, 0, 0, 0, 10).is_none());
    assert!(r.render_tile(0, 1.0, 0, 0, 70_000, 10).is_none());
    assert!(r.render_tile(0, f32::NAN, 0, 0, 10, 10).is_none());
    assert!(r.render_tile(0, -1.0, 0, 0, 10, 10).is_none());
    assert!(r.render_tile(99, 1.0, 0, 0, 10, 10).is_none());
    // Past the page's edge is white, not an error.
    let off = r.render_tile(0, 1.0, 5_000, 5_000, 16, 16).unwrap();
    assert!(off.rgba.iter().all(|&b| b == 255));
}

#[test]
fn text_layer_finds_the_words_and_accents() {
    let doc = sample();
    let r = Renderer::new(&doc);
    let page2 = r.page_text(1).unwrap().plain();
    assert_eq!(page2.matches("lighthouse").count(), 3, "{page2}");
    assert!(page2.contains("café, naïve, Ærøskøbing"), "{page2}");
    let page1 = r.page_text(0).unwrap().plain();
    assert!(page1.contains("Courier: The quick brown fox"), "{page1}");
    assert_eq!(r.page_text(6), None);
}

#[test]
fn documents_are_shared_between_render_threads() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<Document>();

    let doc = Arc::new(sample());
    let workers: Vec<_> = (0..3)
        .map(|n| {
            let doc = Arc::clone(&doc);
            std::thread::spawn(move || {
                let r = Renderer::new(&doc);
                (0..doc.page_count())
                    .map(|i| {
                        r.render_tile(i, 0.5, 0, (n * 64) as u32, 128, 128)
                            .unwrap()
                            .rgba
                            .len()
                    })
                    .sum::<usize>()
            })
        })
        .collect();
    for w in workers {
        assert_eq!(w.join().unwrap(), 6 * 128 * 128 * 4);
    }
}
