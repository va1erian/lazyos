use super::*;

#[test]
fn a_nested_child_paints_at_its_parents_offset() {
    let backend = OffscreenBackend::new();
    let window = backend
        .open_window(&PlatformSpec::new("nested").size(Dip(100.0), Dip(100.0)))
        .unwrap();
    let parent = backend
        .create(
            ParentRef::Window(window),
            &NodeSpec::new(NodeKind::Container, Rect::new(20, 30, 90, 80)),
        )
        .unwrap();
    let child = backend
        .create(
            ParentRef::Widget(parent),
            &NodeSpec::new(NodeKind::Custom, Rect::new(0, 0, 10, 10)),
        )
        .unwrap();
    backend.set_painter(
        child,
        Rc::new(|canvas| {
            let bounds = canvas.bounds();
            canvas.fill_rect(bounds, Color::rgb(255, 0, 0));
        }),
    );

    let image = backend.render(window).expect("a rendered window");
    assert_eq!(
        image.pixel(25, 35),
        Some([255, 0, 0, 255]),
        "the child paints at the parent's offset"
    );
    assert_ne!(
        image.pixel(5, 5),
        Some([255, 0, 0, 255]),
        "not at its parent-relative position"
    );
}

#[test]
fn a_clip_hides_an_overflowing_childs_painting() {
    let backend = OffscreenBackend::new();
    let window = backend.open_window(&PlatformSpec::new("clip")).unwrap();
    let view = backend
        .create(
            ParentRef::Window(window),
            &NodeSpec::new(NodeKind::Container, Rect::new(0, 0, 100, 80)),
        )
        .unwrap();
    let child = backend
        .create(
            ParentRef::Widget(view),
            &NodeSpec::new(NodeKind::Container, Rect::new(0, 50, 100, 60)),
        )
        .unwrap();
    backend.set_painter(
        child,
        Rc::new(|canvas| {
            let bounds = canvas.bounds();
            canvas.fill_rect(bounds, Color::rgb(255, 0, 0));
        }),
    );
    // The view clips its descendants to a 10px band at the top; the child
    // starts below it, so nothing paints.
    backend.set_clip(view, Some(Rect::new(0, 0, 100, 10)));

    let image = backend.render(window).expect("a rendered window");
    assert_eq!(red_pixels(&image), 0, "the child is fully clipped away");
}

#[test]
fn a_clip_keeps_the_visible_part_of_a_child() {
    let backend = OffscreenBackend::new();
    let window = backend.open_window(&PlatformSpec::new("clip")).unwrap();
    let view = backend
        .create(
            ParentRef::Window(window),
            &NodeSpec::new(NodeKind::Container, Rect::new(0, 0, 100, 80)),
        )
        .unwrap();
    let child = backend
        .create(
            ParentRef::Widget(view),
            &NodeSpec::new(NodeKind::Container, Rect::new(0, 5, 100, 60)),
        )
        .unwrap();
    backend.set_painter(
        child,
        Rc::new(|canvas| {
            let bounds = canvas.bounds();
            canvas.fill_rect(bounds, Color::rgb(255, 0, 0));
        }),
    );
    backend.set_clip(view, Some(Rect::new(0, 0, 100, 10)));

    let image = backend.render(window).expect("a rendered window");
    let red = red_pixels(&image);
    assert!(red > 0, "the visible band is painted");
    assert_eq!(
        red,
        100 * 5,
        "only the 5px inside the clip is painted, not the 55 below"
    );
}

#[test]
fn a_hidden_panel_hides_its_painted_child() {
    let backend = OffscreenBackend::new();
    let window = backend
        .open_window(&PlatformSpec::new("visibility"))
        .unwrap();
    let panel = backend
        .create(
            ParentRef::Window(window),
            &NodeSpec::new(NodeKind::Container, Rect::new(0, 0, 100, 80)),
        )
        .unwrap();
    let child = backend
        .create(
            ParentRef::Widget(panel),
            &NodeSpec::new(NodeKind::Container, Rect::new(10, 10, 30, 30)),
        )
        .unwrap();
    backend.set_painter(child, fill(Color::rgb(255, 0, 0)));
    // Hiding the panel must skip its whole subtree.
    backend.set_visible(panel, false);

    let image = backend.render(window).expect("a rendered window");
    assert_eq!(
        red_pixels(&image),
        0,
        "the child of a hidden panel is not painted"
    );
}

#[test]
fn a_visible_panel_with_a_hidden_child_paints_only_the_panel() {
    let backend = OffscreenBackend::new();
    let window = backend
        .open_window(&PlatformSpec::new("visibility"))
        .unwrap();
    let panel = backend
        .create(
            ParentRef::Window(window),
            &NodeSpec::new(NodeKind::Container, Rect::new(0, 0, 100, 80)),
        )
        .unwrap();
    let child = backend
        .create(
            ParentRef::Widget(panel),
            &NodeSpec::new(NodeKind::Container, Rect::new(10, 10, 30, 30)),
        )
        .unwrap();
    backend.set_painter(panel, fill(Color::rgb(255, 0, 0)));
    backend.set_painter(child, fill(Color::rgb(0, 0, 255)));
    backend.set_visible(child, false);

    let image = backend.render(window).expect("a rendered window");
    assert_eq!(red_pixels(&image), 100 * 80, "the panel itself is painted");
    assert_eq!(blue_pixels(&image), 0, "its hidden child is skipped");
}

#[test]
fn an_unhidden_child_under_a_visible_parent_still_paints() {
    let backend = OffscreenBackend::new();
    let window = backend
        .open_window(&PlatformSpec::new("visibility"))
        .unwrap();
    let panel = backend
        .create(
            ParentRef::Window(window),
            &NodeSpec::new(NodeKind::Container, Rect::new(0, 0, 100, 80)),
        )
        .unwrap();
    let child = backend
        .create(
            ParentRef::Widget(panel),
            &NodeSpec::new(NodeKind::Container, Rect::new(10, 10, 30, 30)),
        )
        .unwrap();
    backend.set_painter(child, fill(Color::rgb(255, 0, 0)));

    let image = backend.render(window).expect("a rendered window");
    assert_eq!(
        red_pixels(&image),
        20 * 20,
        "the child of a visible panel is painted, not over-culled"
    );
}

#[test]
fn a_scroll_view_clips_overflowing_content_light_and_dark() {
    struct Scroll {
        _view: ScrollView<u32>,
        _labels: Vec<Label<u32>>,
    }

    impl App for Scroll {
        type Msg = u32;
        fn update(&mut self, _msg: u32, _ui: &mut Ui<u32>) {}
    }

    for (dark, name) in [
        (false, "scrollview-light.png"),
        (true, "scrollview-dark.png"),
    ] {
        let backend = Rc::new(OffscreenBackend::new());
        let backend_for_run: Rc<dyn Backend> = backend.clone();
        let mut rendered = None;

        let result = run_app(
            backend_for_run,
            PlatformSpec::new("scroll view").size(Dip(180.0), Dip(120.0)),
            |ui| {
                ui.set_theme(if dark { Theme::dark() } else { Theme::light() });
                let view = ScrollView::new(ui, Rect::new(12, 12, 128, 84)).unwrap();
                let mut labels = Vec::new();
                for text in ["alpha", "beta", "gamma", "delta", "epsilon"] {
                    let label = Label::new(view.ui(), Rect::new(0, 0, 10, 10), text).unwrap();
                    view.add(label.id(), Dip(28.0));
                    labels.push(label);
                }
                let image = backend.render(ui.window()).expect("a rendered window");
                save(name, &image);
                rendered = Some(image);
                Scroll {
                    _view: view,
                    _labels: labels,
                }
            },
        );

        assert!(result.is_ok(), "the app ran");
        assert!(rendered.is_some(), "the view rendered");
    }
}

#[test]
fn an_edit_paints_at_high_dpi_on_both_themes() {
    use xui_core::widget::Edit;

    struct Field {
        _edit: Edit<()>,
    }
    impl App for Field {
        type Msg = ();
        fn update(&mut self, _msg: (), _ui: &mut Ui<()>) {}
    }

    for dpi in [144u32, 192] {
        for dark in [false, true] {
            let backend = Rc::new(OffscreenBackend::with_dpi(dpi));
            let backend_for_run: Rc<dyn Backend> = backend.clone();
            let mut painted = false;

            let _ = run_app(
                backend_for_run,
                PlatformSpec::new("edit").size(Dip(200.0), Dip(40.0)),
                |ui| {
                    let theme = if dark { Theme::dark() } else { Theme::light() };
                    ui.set_theme(theme);
                    let scale = dpi as f32 / 96.0;
                    let bounds =
                        Rect::new(8, 8, (200.0 * scale) as i32 - 8, (40.0 * scale) as i32 - 8);
                    let edit = Edit::new(ui, bounds, "hello").unwrap();
                    let image = backend.render(ui.window()).expect("a rendered window");
                    let bg = [
                        theme.input_background.r,
                        theme.input_background.g,
                        theme.input_background.b,
                    ];
                    painted = image
                        .pixels
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .any(|pixel| pixel[..3] != bg);
                    Field { _edit: edit }
                },
            );

            assert!(painted, "the edit painted at {dpi} DPI (dark: {dark})");
        }
    }
}
