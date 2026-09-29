use super::*;

#[test]
fn capture_returns_the_rendered_surface() {
    let backend = OffscreenBackend::new();
    let window = backend
        .open_window(&PlatformSpec::new("shot").size(Dip(60.0), Dip(40.0)))
        .unwrap();
    let node = backend
        .create(
            ParentRef::Window(window),
            &NodeSpec::new(NodeKind::Custom, Rect::new(0, 0, 60, 40)),
        )
        .unwrap();
    backend.set_painter(
        node,
        Rc::new(|canvas| {
            let bounds = canvas.bounds();
            canvas.fill_rect(bounds, Color::rgb(255, 0, 0));
        }),
    );

    let image = backend.capture(window).expect("the window was captured");
    assert_eq!(image.size(), (60, 40));
    assert_eq!(image.pixel(30, 20), Some([255, 0, 0, 255]));
}

#[test]
fn a_painter_capturing_its_own_window_does_not_recurse() {
    let backend = Rc::new(OffscreenBackend::new());
    let window = backend
        .open_window(&PlatformSpec::new("reentrant capture"))
        .unwrap();
    let node = backend
        .create(
            ParentRef::Window(window),
            &NodeSpec::new(NodeKind::Custom, Rect::new(0, 0, 10, 10)),
        )
        .unwrap();
    // The painter asks the backend to capture the very window being painted; the
    // nested render must be refused, not recurse until the stack overflows.
    let nested = Rc::new(RefCell::new(None));
    let backend_for_painter = Rc::clone(&backend);
    let nested_for_painter = Rc::clone(&nested);
    backend.set_painter(
        node,
        Rc::new(move |_canvas| {
            *nested_for_painter.borrow_mut() = Some(backend_for_painter.capture(window).is_err());
        }),
    );

    let image = backend.render(window);
    assert!(image.is_some(), "the outer render completes");
    assert_eq!(
        *nested.borrow(),
        Some(true),
        "the nested capture is refused"
    );
}

#[test]
fn capturing_a_closed_window_is_an_error() {
    let backend = OffscreenBackend::new();
    let window = backend.open_window(&PlatformSpec::new("gone")).unwrap();
    backend.close_window(window);
    assert!(backend.capture(window).is_err());
}
