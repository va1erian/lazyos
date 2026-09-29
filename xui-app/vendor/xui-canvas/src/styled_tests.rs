#![forbid(unsafe_code)]

//! Text and packed-widget rendering tests kept out of the main `tests` module
//! so neither file grows past the repo's file-size limit.

use std::rc::Rc;

use crate::tests::{dark_pixels, save};
use crate::{OffscreenBackend, RgbaImage, Surface, measure_text};
use xui_core::app::{App, Ui, run_app};
use xui_core::backend::{Backend, PlatformSpec};
use xui_core::geometry::Rect;
use xui_core::widget::{Button, FlowText, Icon, Run};
use xui_core::{Canvas, Color, Dip, TextStyle, Theme};

/// The rightmost x that carries ink darker than a light background.
fn rightmost_ink(image: &RgbaImage) -> i32 {
    let mut right = -1;
    for y in 0..image.height {
        for x in 0..image.width {
            let Some([r, g, b, _]) = image.pixel(x, y) else {
                continue;
            };
            if r < 200 && g < 200 && b < 200 {
                right = right.max(x as i32);
            }
        }
    }
    right
}

/// Pixels whose blue channel dominates, proving a coloured run was painted.
fn blue_pixels(image: &RgbaImage) -> usize {
    image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| i32::from(pixel[2]) > i32::from(pixel[0]) + 20)
        .count()
}

#[test]
fn bold_italic_coloured_text_renders_and_measures() {
    let style = TextStyle::new(Color::hex(0x10_60_C0), Dip(20.0))
        .weight(700)
        .italic(true);
    let mut surface = Surface::new(320, 72);
    surface.fill(Color::rgb(255, 255, 255));
    surface.with_canvas(Rect::new(0, 0, 320, 72), |canvas| {
        canvas.draw_text("Styled", Rect::new(8, 8, 312, 64), &style);
    });

    let image = surface.to_image();
    assert!(
        blue_pixels(&image) > 100,
        "the coloured glyphs were painted: {}",
        blue_pixels(&image)
    );
    let measured = measure_text("Styled", &style, 96, 1000).width;
    let ink = rightmost_ink(&image);
    assert!(ink > 0, "the glyphs left ink");
    // An italic glyph leans past its advance by a fraction of the em size.
    assert!(
        ink <= measured + measured / 3,
        "the ink ({ink}) stays near the measured advance ({measured})"
    );
    let regular = measure_text("Styled", &TextStyle::new(style.color, Dip(20.0)), 96, 1000).width;
    assert!(
        measured > regular,
        "the bold face measures wider than the regular one: {measured} vs {regular}"
    );
    save("canvas-styled-text.png", &image);
}

/// Keeps the widgets alive for the duration of a `run_app` call.
struct FlowDemo {
    _flow: FlowText<u32>,
    _button: Button<u32>,
}

impl App for FlowDemo {
    type Msg = u32;
    fn update(&mut self, _msg: u32, _ui: &mut Ui<u32>) {}
}

#[test]
fn flow_text_and_icon_render_light_and_dark() {
    let backend = Rc::new(OffscreenBackend::new());
    let backend_for_run: Rc<dyn Backend> = backend.clone();
    let mut light: Option<RgbaImage> = None;
    let mut dark: Option<RgbaImage> = None;

    let _ = run_app(
        backend_for_run,
        PlatformSpec::new("flow").size(Dip(340.0), Dip(120.0)),
        |ui| {
            let flow = FlowText::new(ui, Rect::new(12, 12, 328, 64))
                .unwrap()
                .run(Run::link("The Midnight Set").on_click(|| Some(1)))
                .separator(" · ")
                .run(Run::normal("Signal 1"))
                .separator(" · ")
                .run(Run::weak("(2004)"));
            let button = Button::new(ui, Rect::new(12, 76, 150, 108), "Add")
                .unwrap()
                .icon(Icon::Plus);

            let light_image = backend.render(ui.window()).expect("a light render");
            save("flow-text-light.png", &light_image);
            light = Some(light_image);

            ui.set_theme(Theme::dark());
            let dark_image = backend.render(ui.window()).expect("a dark render");
            save("flow-text-dark.png", &dark_image);
            dark = Some(dark_image);

            FlowDemo {
                _flow: flow,
                _button: button,
            }
        },
    );

    let light = light.expect("the light render");
    let dark = dark.expect("the dark render");
    assert!(dark_pixels(&light) > 50, "the runs painted on light");
    assert!(
        blue_pixels(&light) > 10,
        "the link's accent painted on light"
    );
    assert!(dark_pixels(&dark) > 50, "the runs painted on dark");
}
