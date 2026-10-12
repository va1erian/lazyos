//! The P4 drawing building blocks through the public API, with PNGs written to
//! `target/snapshots/` for eyeballing (soft brush, antialiased shapes,
//! gradients, text).

use xui_paint::model::tools::{
    Arrow, BrushOptions, FillMode, Gradient, GradientKind, GradientOptions, PaintMode,
    ShapeOptions, StrokeBuffer, pixel_center, shapes,
};
use xui_paint::model::{Bitmap, Rect};

const WHITE: [u8; 4] = [255, 255, 255, 255];

fn save(name: &str, bitmap: &Bitmap) {
    std::fs::create_dir_all("target/snapshots").unwrap();
    std::fs::write(
        format!("target/snapshots/{name}.png"),
        bitmap.encode_png().unwrap(),
    )
    .unwrap();
}

fn opts(width: f32, mode: FillMode, aa: bool) -> ShapeOptions {
    ShapeOptions {
        width,
        mode,
        antialias: aa,
        ..ShapeOptions::default()
    }
}

#[test]
fn soft_brush_strokes() {
    let mut layer = Bitmap::white(360, 260);
    let curve = |i: usize| (30.0 + i as f32 * 3.0, 40.0 + (i as f32 * 0.12).sin() * 18.0);
    for (row, hardness) in [100u8, 60, 25, 0].into_iter().enumerate() {
        let mut stroke = StrokeBuffer::new(&layer);
        let brush = BrushOptions {
            width: 36,
            hardness,
            antialias: true,
        };
        let shift = row as f32 * 62.0;
        for i in 0..100 {
            let (a, b) = (curve(i), curve(i + 1));
            stroke.segment(&layer, (a.0, a.1 + shift), (b.0, b.1 + shift), brush);
            stroke.composite(&mut layer, [20, 60, 200, 200], PaintMode::Paint, None);
        }
    }
    save("paint-p4-soft-brush", &layer);
    // Off the centre line a hard stroke is still solid and a soft one has faded.
    let (x, y) = curve(40);
    let hard = layer.get(x as i32, y as i32 + 14).unwrap();
    let soft = layer.get(x as i32, y as i32 + 3 * 62 + 14).unwrap();
    assert!(hard[0] < soft[0], "{hard:?} vs {soft:?}");
}

#[test]
fn antialiased_and_aliased_shapes() {
    let area = Rect::new(0, 0, 400, 200);
    let mut layer = Bitmap::white(400, 200);
    let red = [200, 30, 30, 255];
    let blue = [60, 120, 230, 255];
    for (column, aa) in [(0, true), (200, false)] {
        let shift = |p: (i32, i32)| (p.0 + column, p.1);
        let both = opts(5.0, FillMode::Both, aa);
        shapes::rectangle(shift((10, 10)), shift((90, 60)), &both, area)
            .paint(&mut layer, red, blue, None);
        let rounded = ShapeOptions {
            radius: 16.0,
            ..both
        };
        shapes::rectangle(shift((100, 10)), shift((180, 60)), &rounded, area)
            .paint(&mut layer, red, blue, None);
        shapes::ellipse(shift((10, 75)), shift((90, 145)), &both, area)
            .paint(&mut layer, red, blue, None);
        shapes::ellipse(
            shift((100, 75)),
            shift((180, 125)),
            &opts(3.0, FillMode::Outline, aa),
            area,
        )
        .paint(&mut layer, red, blue, None);
        let arrow = ShapeOptions {
            arrow: Arrow::Both,
            ..opts(3.0, FillMode::Outline, aa)
        };
        let from = pixel_center(10 + column, 170);
        let to = pixel_center(180 + column, 150);
        if let Some(line) = shapes::line(from, to, &arrow, area) {
            let mut stroke = StrokeBuffer::new(&layer);
            stroke.add_coverage(&layer, &line);
            stroke.composite(&mut layer, [20, 20, 20, 255], PaintMode::Paint, None);
        }
    }
    save("paint-p4-shapes", &layer);
    assert!(layer.get(50, 35).unwrap() != WHITE);
}

#[test]
fn gradients() {
    let mut layer = Bitmap::white(480, 120);
    let kinds = [
        GradientKind::Linear,
        GradientKind::LinearReflected,
        GradientKind::Radial,
        GradientKind::Diamond,
    ];
    for (i, kind) in kinds.into_iter().enumerate() {
        let x = i as i32 * 120;
        let g = Gradient::new(
            GradientOptions { kind },
            (x as f32 + 60.0, 60.0),
            (x as f32 + 110.0, 90.0),
            [255, 200, 0, 255],
            [20, 40, 160, 90],
        );
        g.paint(&mut layer, Rect::new(x, 0, 120, 120), None);
    }
    save("paint-p4-gradients", &layer);
    assert!(layer.get(60, 60).unwrap()[0] > 200);
}

#[cfg(feature = "text")]
#[test]
fn text_snapshot() {
    use xui_paint::model::text::{CosmicRasterizer, TextAlign, TextEdit, TextStyle};
    let fonts = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../assets/fonts/liberation/"
    );
    let mut r = CosmicRasterizer::new();
    for name in [
        "LiberationSans-Regular",
        "LiberationSans-Bold",
        "LiberationSerif-Italic",
    ] {
        r.register_font(std::fs::read(format!("{fonts}{name}.ttf")).unwrap());
    }
    let mut layer = Bitmap::white(420, 220);
    let mut y = 8;
    for (style, text) in [
        (TextStyle::default(), "Hello, Paint!"),
        (
            TextStyle {
                bold: true,
                underline: true,
                ..TextStyle::default()
            },
            "Bold underlined",
        ),
        (
            TextStyle {
                family: "Liberation Serif".into(),
                italic: true,
                size: 30.0,
                align: TextAlign::Center,
                ..TextStyle::default()
            },
            "Centred serif\nitalic lines",
        ),
        (
            TextStyle {
                size: 20.0,
                antialias: false,
                ..TextStyle::default()
            },
            "Aliased text",
        ),
    ] {
        let mut edit = TextEdit::new((10, y), style);
        edit.insert_str(text);
        let cover = edit.commit(&r).expect("ink");
        let mut stroke = StrokeBuffer::new(&layer);
        stroke.add_coverage(&layer, &cover);
        stroke.composite(&mut layer, [30, 30, 30, 255], PaintMode::Paint, None);
        y += cover.rect().height() as i32 + 6;
    }
    save("paint-p4-text", &layer);
}
