use alloc::string::String;
use alloc::vec::Vec;

use super::*;

fn triangle() -> ShapeDef {
    ShapeDef {
        hot: (16, 16),
        layers: alloc::vec![LayerDef {
            polys: alloc::vec![alloc::vec![(16, 16), (400, 200), (200, 400)]],
            fill: 0x2060e0,
            outline: 0x101010,
            thin: false,
        }],
    }
}

#[test]
fn the_builtin_theme_round_trips() {
    let theme = Theme::builtin("builtin");
    let text = format::serialize(&theme);
    assert_eq!(format::parse(&text).unwrap(), theme);
}

#[test]
fn the_builtin_theme_draws_the_builtin_shapes() {
    let theme = Theme::builtin("builtin");
    for shape in Shape::ALL {
        for px in [16, 32, 64] {
            assert_eq!(
                render_themed(&theme, shape, px, 0),
                render(shape, px, 0),
                "{shape:?} {px}"
            );
        }
    }
}

#[test]
fn a_custom_shape_replaces_only_its_slot() {
    let mut theme = Theme {
        name: "blue".into(),
        ..Theme::default()
    };
    theme.shapes[Shape::Default as usize] = Some(triangle());
    let arrow = render_themed(&theme, Shape::Default, 32, 0);
    assert_ne!(arrow, render(Shape::Default, 32, 0));
    assert!(arrow
        .pixels
        .iter()
        .any(|&p| p & 0xff > (p >> 16) & 0xff && p >> 24 == 255));
    assert_eq!(
        render_themed(&theme, Shape::Hand, 32, 0),
        render(Shape::Hand, 32, 0)
    );
    let m = margin(32);
    assert_eq!((arrow.hot_x, arrow.hot_y), (m + 1, m + 1));
    assert!(arrow.w <= max_extent(32));
}

#[test]
fn an_invalid_definition_is_ignored_when_rendering() {
    let mut theme = Theme::default();
    let mut bad = triangle();
    bad.layers[0].polys[0][0] = (9999, 0);
    theme.shapes[Shape::Text as usize] = Some(bad);
    assert_eq!(
        render_themed(&theme, Shape::Text, 32, 0),
        render(Shape::Text, 32, 0)
    );
}

#[test]
fn refuses_bad_files_with_a_line() {
    let cases = [
        ("", "not a lazyos-cursor 1 file"),
        ("lazyos-cursor 2\n", "not a lazyos-cursor 1 file"),
        ("lazyos-cursor 1\n", "missing name"),
        ("lazyos-cursor 1\nname Bad\n", "bad theme name"),
        ("lazyos-cursor 1\nname a\nshape nope\n", "unknown shape"),
        ("lazyos-cursor 1\nname a\nlayer fill 000000 outline 000000\n", "layer outside a shape"),
        ("lazyos-cursor 1\nname a\nshape text\nlayer fill 00000g outline 000000\n", "layer needs fill and outline as RRGGBB"),
        ("lazyos-cursor 1\nname a\nshape text\nlayer fill +00000 outline 000000\n", "layer needs fill and outline as RRGGBB"),
        ("lazyos-cursor 1\nname a\nshape text\nlayer fill 000000 outline 000000\npoly 1 2 3\n", "poly needs 3+ x y pairs"),
        ("lazyos-cursor 1\nname a\nshape text\nlayer fill 000000 outline 000000\npoly 0 0 513 0 0 5\n", "coordinate outside 0..=512"),
        ("lazyos-cursor 1\nname a\nshape text\nlayer fill 000000 outline 000000\npoly -1 0 5 0 0 5\n", "coordinate outside 0..=512"),
        ("lazyos-cursor 1\nname a\nshape text\n", "shape needs a polygon of 3+ points"),
        ("lazyos-cursor 1\nname a\nbogus\n", "unknown directive"),
    ];
    for (text, what) in cases {
        let error = format::parse(text).unwrap_err();
        assert_eq!(error.what, what, "{text:?}");
    }
}

#[test]
fn refuses_a_shape_given_twice_and_too_many_vertices() {
    let one = "shape text\nlayer fill 000000 outline 000000\npoly 0 0 9 0 0 9\n";
    let twice = alloc::format!("lazyos-cursor 1\nname a\n{one}{one}");
    assert_eq!(format::parse(&twice).unwrap_err().what, "shape given twice");

    let mut big =
        String::from("lazyos-cursor 1\nname a\nshape text\nlayer fill 000000 outline 000000\n");
    for _ in 0..(theme::MAX_VERTICES / 3 + 1) {
        big.push_str("poly 0 0 9 0 0 9\n");
    }
    assert_eq!(format::parse(&big).unwrap_err().what, "too many vertices");
}

#[test]
fn comments_and_blank_lines_are_skipped() {
    let text = "# a theme\n\nlazyos-cursor 1\n# name\nname tiny\n";
    assert_eq!(format::parse(text).unwrap().name, "tiny");
}

#[test]
fn garbage_never_panics() {
    let base = format::serialize(&Theme::builtin("builtin"));
    let bytes = base.as_bytes();
    let mut seed = 0x2545_f491u32;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for _ in 0..400 {
        let mut copy: Vec<u8> = bytes.to_vec();
        for _ in 0..(next() % 8 + 1) {
            let at = next() as usize % copy.len();
            copy[at] = (next() % 128) as u8;
        }
        copy.truncate(next() as usize % (copy.len() + 1));
        if let Ok(text) = core::str::from_utf8(&copy) {
            if let Ok(theme) = format::parse(text) {
                for shape in Shape::ALL {
                    let _ = render_themed(&theme, shape, 16, 0);
                }
            }
        }
    }
}

#[test]
fn names_are_checked() {
    assert!(theme::valid_name("midnight-2"));
    for bad in ["", "Up", "a b", "../x", "a/b", &"x".repeat(33)] {
        assert!(!theme::valid_name(bad), "{bad:?}");
    }
}

#[test]
fn an_empty_layer_is_allowed_but_a_shape_must_draw_something() {
    let mut def = triangle();
    def.layers.push(LayerDef {
        polys: Vec::new(),
        fill: 0,
        outline: 0,
        thin: false,
    });
    assert!(def.is_valid());
    let mut theme = Theme {
        name: "t".into(),
        ..Theme::default()
    };
    theme.shapes[Shape::Busy as usize] = Some(def);
    assert!(theme.draws(Shape::Busy));
    assert!(!theme.draws(Shape::Hand));
    let text = format::serialize(&theme);
    assert_eq!(format::parse(&text).unwrap(), theme);
    theme.shapes[Shape::Busy as usize]
        .as_mut()
        .unwrap()
        .layers
        .remove(0);
    assert!(!theme.draws(Shape::Busy), "no polygon anywhere");
}
