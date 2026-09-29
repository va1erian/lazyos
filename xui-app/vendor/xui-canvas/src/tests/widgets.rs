use super::*;

/// Keeps the widgets alive for the duration of a `run_app` call.
struct Widgets {
    _button: Button<u32>,
    _check: CheckBox<u32>,
    _bar: ProgressBar<u32>,
    _slider: Slider<u32>,
    _label: Label<u32>,
}

impl App for Widgets {
    type Msg = u32;
    fn update(&mut self, _msg: u32, _ui: &mut Ui<u32>) {}
}

fn contains(image: &RgbaImage, color: [u8; 3]) -> bool {
    image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[..3] == color)
}

#[test]
fn portable_widgets_render_on_the_software_backend() {
    let backend = Rc::new(OffscreenBackend::new());
    let backend_for_run: Rc<dyn Backend> = backend.clone();
    let accent = Color::hex(0x00_5F_B8);
    let mut captured: Option<RgbaImage> = None;

    let _ = run_app(
        backend_for_run,
        PlatformSpec::new("canvas widgets").size(Dip(420.0), Dip(200.0)),
        |ui| {
            let button = Button::new(ui, Rect::new(20, 20, 180, 52), "Click").unwrap();
            let check = CheckBox::new(ui, Rect::new(20, 64, 240, 92), "Enabled").unwrap();
            check.set_checked(true);
            let bar = ProgressBar::new(ui, Rect::new(20, 104, 380, 112), 100).unwrap();
            bar.set_value(60);
            let slider = Slider::new(ui, Rect::new(20, 128, 380, 156), 0.0, 100.0).unwrap();
            slider.set_value(40.0);
            let label = Label::new(ui, Rect::new(20, 168, 380, 196), "xui on tiny-skia").unwrap();

            let image = backend.render(ui.window()).expect("a rendered window");
            assert!(
                contains(&image, [accent.r, accent.g, accent.b]),
                "the accent colour is on the surface"
            );
            save("canvas-widgets.png", &image);
            captured = Some(image);

            Widgets {
                _button: button,
                _check: check,
                _bar: bar,
                _slider: slider,
                _label: label,
            }
        },
    );

    assert!(captured.is_some(), "the surface was rendered");
}

#[test]
fn a_design_length_is_converted_once_at_each_dpi() {
    let backend = OffscreenBackend::new();
    // A 10 DIP square is 10px at 100% and 20px at 200%. If the boundary scaled
    // it twice it would cover 40px at 200%, so probing 25px catches that.
    for (dpi, covered, outside) in [(96u32, 5u32, 15u32), (192u32, 15u32, 25u32)] {
        let window = backend
            .open_window_at(&PlatformSpec::new("dpi"), dpi)
            .unwrap();
        assert_eq!(backend.dpi(window), dpi, "the window renders at {dpi}");

        let node = backend
            .create(
                ParentRef::Window(window),
                &NodeSpec::new(NodeKind::Custom, backend.client_rect(window)),
            )
            .unwrap();
        backend.set_painter(
            node,
            Rc::new(move |canvas| {
                let side = Dip(10.0).to_px(canvas.dpi()).value();
                canvas.fill_rect(Rect::new(0, 0, side, side), Color::rgb(255, 0, 0));
            }),
        );

        let image = backend.render(window).expect("a rendered window");
        assert_eq!(
            image.pixel(covered, covered),
            Some([255, 0, 0, 255]),
            "a 10 DIP square covers {covered}px at {dpi} DPI"
        );
        assert_ne!(
            image.pixel(outside, outside),
            Some([255, 0, 0, 255]),
            "and not {outside}px, so the DIP was scaled exactly once"
        );
    }
}
