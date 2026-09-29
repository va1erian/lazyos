use super::*;

#[test]
fn a_filled_rectangle_lands_on_the_surface() {
    let mut surface = Surface::new(64, 64);
    surface.fill(Color::rgb(255, 255, 255));
    surface.with_canvas(Rect::new(0, 0, 64, 64), |canvas| {
        canvas.fill_rect(Rect::new(16, 16, 48, 48), Color::rgb(255, 0, 0));
    });

    let image = surface.to_image();
    assert_eq!(image.pixel(32, 32), Some([255, 0, 0, 255]));
    assert_eq!(image.pixel(4, 4), Some([255, 255, 255, 255]));
}

#[test]
fn a_translated_rectangle_moves() {
    let mut surface = Surface::new(64, 64);
    surface.fill(Color::rgb(0, 0, 0));
    surface.with_canvas(Rect::new(0, 0, 64, 64), |canvas| {
        canvas.set_translation(0.0, -8.0);
        canvas.fill_rect(Rect::new(0, 8, 32, 24), Color::rgb(0, 255, 0));
    });

    let image = surface.to_image();
    assert_eq!(image.pixel(10, 8), Some([0, 255, 0, 255]));
}

#[test]
fn a_ellipse_does_not_fill_its_bounding_corners() {
    let mut surface = Surface::new(64, 64);
    surface.fill(Color::rgb(0, 0, 0));
    surface.with_canvas(Rect::new(0, 0, 64, 64), |canvas| {
        canvas.fill_ellipse(Point::new(32, 32), 24.0, 24.0, Color::rgb(0, 128, 255));
    });

    let image = surface.to_image();
    assert_eq!(
        image.pixel(32, 32),
        Some([0, 128, 255, 255]),
        "centre filled"
    );
    assert_eq!(
        image.pixel(9, 9),
        Some([0, 0, 0, 255]),
        "corner stays clear"
    );
}

#[test]
fn a_push_clip_limits_drawing() {
    let mut surface = Surface::new(64, 64);
    surface.fill(Color::rgb(0, 0, 0));
    surface.with_canvas(Rect::new(0, 0, 64, 64), |canvas| {
        canvas.push_clip(Rect::new(0, 0, 32, 64));
        canvas.fill_rect(Rect::new(0, 0, 64, 64), Color::rgb(255, 255, 0));
        canvas.pop_clip();
    });

    let image = surface.to_image();
    assert_eq!(image.pixel(16, 32), Some([255, 255, 0, 255]), "inside clip");
    assert_eq!(image.pixel(48, 32), Some([0, 0, 0, 255]), "outside clip");
}

#[test]
fn an_alpha_fill_blends_with_the_background() {
    let mut surface = Surface::new(64, 64);
    surface.fill(Color::rgb(255, 255, 255));
    surface.with_canvas(Rect::new(0, 0, 64, 64), |canvas| {
        canvas.fill_rect_rgba(Rect::new(0, 0, 64, 64), Rgba::with_alpha(255, 0, 0, 128));
    });

    let image = surface.to_image();
    let [r, g, b, a] = image.pixel(32, 32).unwrap();
    assert_eq!((r, a), (255, 255), "red stays red and opaque: {image:?}");
    for (channel, value) in [("green", g), ("blue", b)] {
        assert!(
            (100..=170).contains(&value),
            "the white background lightened the {channel} channel: {value}"
        );
    }
}

#[test]
fn a_rounded_corner_is_clipped() {
    let mut surface = Surface::new(64, 64);
    surface.fill(Color::rgb(0, 0, 0));
    surface.with_canvas(Rect::new(0, 0, 64, 64), |canvas| {
        canvas.fill_rounded_rect_corners(
            Rect::new(16, 16, 48, 48),
            [Corner::uniform(12.0); 4],
            Rgba::rgb(255, 0, 0),
        );
    });

    let image = surface.to_image();
    assert_eq!(image.pixel(32, 32), Some([255, 0, 0, 255]), "centre filled");
    assert_eq!(
        image.pixel(17, 17),
        Some([0, 0, 0, 255]),
        "the corner is clipped away"
    );
}

#[test]
fn a_rounded_clip_rounds_off_its_corners() {
    let mut surface = Surface::new(64, 64);
    surface.fill(Color::rgb(0, 0, 0));
    surface.with_canvas(Rect::new(0, 0, 64, 64), |canvas| {
        canvas.push_clip_rounded(Rect::new(8, 8, 56, 56), [Corner::uniform(16.0); 4]);
        canvas.fill_rect(Rect::new(0, 0, 64, 64), Color::rgb(0, 255, 0));
        canvas.pop_clip();
    });

    let image = surface.to_image();
    assert_eq!(image.pixel(32, 32), Some([0, 255, 0, 255]), "inside clip");
    assert_eq!(
        image.pixel(9, 9),
        Some([0, 0, 0, 255]),
        "the rounded corner stays clear"
    );
}

#[test]
fn a_dashed_line_leaves_gaps() {
    let mut surface = Surface::new(64, 64);
    surface.fill(Color::rgb(0, 0, 0));
    surface.with_canvas(Rect::new(0, 0, 64, 64), |canvas| {
        canvas.draw_line_stroked(
            Point::new(2, 32),
            Point::new(62, 32),
            Rgba::rgb(255, 255, 255),
            &Stroke::new(2.0).dash(Dash::Dashed),
        );
    });

    let image = surface.to_image();
    let lit = (2..62)
        .filter(|x| image.pixel(*x, 32).unwrap()[0] > 128)
        .count();
    let dark = (2..62)
        .filter(|x| image.pixel(*x, 32).unwrap()[0] <= 128)
        .count();
    assert!(
        lit > 0 && dark > 0,
        "a dashed line has both dashes ({lit}) and gaps ({dark})"
    );
}

#[test]
fn a_two_stop_gradient_varies_across_the_rect() {
    let mut surface = Surface::new(64, 64);
    surface.fill(Color::rgb(255, 0, 255));
    surface.with_canvas(Rect::new(0, 0, 64, 64), |canvas| {
        canvas.fill_rect_linear(
            Rect::new(0, 0, 64, 64),
            &LinearGradient::new(
                Point::new(0, 0),
                Point::new(64, 0),
                vec![
                    GradientStop::new(0.0, Rgba::rgb(0, 0, 0)),
                    GradientStop::new(1.0, Rgba::rgb(255, 255, 255)),
                ],
            ),
        );
    });

    let image = surface.to_image();
    let left = image.pixel(4, 32).unwrap()[0];
    let right = image.pixel(60, 32).unwrap()[0];
    assert!(
        left < right,
        "the gradient runs dark to light: {left} vs {right}"
    );
}

#[test]
fn a_radial_gradient_fades_from_the_centre() {
    let mut surface = Surface::new(64, 64);
    surface.fill(Color::rgb(0, 0, 0));
    surface.with_canvas(Rect::new(0, 0, 64, 64), |canvas| {
        canvas.fill_rect_radial(
            Rect::new(0, 0, 64, 64),
            &RadialGradient::new(
                Point::new(32, 32),
                24.0,
                24.0,
                vec![
                    GradientStop::new(0.0, Rgba::rgb(255, 255, 255)),
                    GradientStop::new(1.0, Rgba::rgb(0, 0, 0)),
                ],
            ),
        );
    });

    let image = surface.to_image();
    let centre = image.pixel(32, 32).unwrap()[0];
    let edge = image.pixel(9, 9).unwrap()[0];
    assert!(
        centre > edge,
        "the radial gradient is bright at the centre: {centre} vs {edge}"
    );
}

#[test]
fn skia_colour_is_opaque() {
    let color = to_skia(Color::rgb(0x12, 0x34, 0x56));
    assert_eq!(color.alpha(), 1.0);
}

#[test]
fn a_drawn_image_lands_on_the_surface() {
    let image = Image::from_rgba(1, 1, vec![255, 0, 0, 255]).expect("image");

    let mut surface = Surface::new(32, 32);
    surface.fill(Color::rgb(0, 0, 0));
    surface.with_canvas(Rect::new(0, 0, 32, 32), |canvas| {
        canvas.draw_image(&image, Rect::new(8, 8, 24, 24));
    });

    let rendered = surface.to_image();
    assert_eq!(
        rendered.pixel(16, 16),
        Some([255, 0, 0, 255]),
        "image lands"
    );
    assert_eq!(
        rendered.pixel(2, 2),
        Some([0, 0, 0, 255]),
        "outside stays clear"
    );
}

#[test]
fn a_repeat_draw_reuses_the_cached_upload() {
    // A 4x4 checkerboard, so a scaled blit has real pixels to land.
    let mut pixels = Vec::new();
    for y in 0..4 {
        for x in 0..4 {
            let channel = if (x + y) % 2 == 0 { 0xF0 } else { 0x10 };
            pixels.extend_from_slice(&[channel, channel, channel, 255]);
        }
    }
    let image = Image::from_rgba(4, 4, pixels).expect("image");

    // Repaint the same surface twice: the second draw must come from the
    // cached upload and land exactly the same pixels.
    let mut surface = Surface::new(16, 16);
    let paint = |surface: &mut Surface| {
        surface.fill(Color::rgb(0, 0, 0));
        surface.with_canvas(Rect::new(0, 0, 16, 16), |canvas| {
            canvas.draw_image(&image, Rect::new(0, 0, 16, 16));
        });
        surface.to_image()
    };
    let first = paint(&mut surface);
    let second = paint(&mut surface);

    assert!(first.pixel(0, 0).is_some_and(|p| p[0] > 0xC0));
    assert_eq!(
        first.pixels, second.pixels,
        "the cached draw must land the same pixels"
    );
}
