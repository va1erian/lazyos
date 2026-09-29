use super::*;

#[test]
fn a_nested_child_is_hit_at_its_absolute_position() {
    let backend = OffscreenBackend::new();
    let window = backend
        .open_window(&PlatformSpec::new("nested hit"))
        .unwrap();
    let parent = backend
        .create(
            ParentRef::Window(window),
            &NodeSpec::new(NodeKind::Container, Rect::new(20, 30, 60, 40)),
        )
        .unwrap();
    let child = backend
        .create(
            ParentRef::Widget(parent),
            &NodeSpec::new(NodeKind::Container, Rect::new(5, 5, 10, 10)),
        )
        .unwrap();
    let log = Rc::new(RefCell::new(Vec::new()));
    backend.set_event_sink(window, Rc::new(Recorder(Rc::clone(&log))));

    let consumed = backend.inject(
        window,
        Event::MouseDown {
            x: 27,
            y: 37,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        },
    );

    assert!(consumed, "the child consumes the click");
    assert!(
        matches!(
            log.borrow().last(),
            Some((target, Event::MouseDown { x: 2, y: 2, .. })) if *target == child
        ),
        "the click targets the child at (25, 35) and is translated locally"
    );
}

#[test]
fn a_child_under_a_hidden_ancestor_is_not_hit() {
    let backend = OffscreenBackend::new();
    let window = backend
        .open_window(&PlatformSpec::new("hidden hit"))
        .unwrap();
    let parent = backend
        .create(
            ParentRef::Window(window),
            &NodeSpec::new(NodeKind::Container, Rect::new(20, 30, 60, 40)),
        )
        .unwrap();
    let _child = backend
        .create(
            ParentRef::Widget(parent),
            &NodeSpec::new(NodeKind::Container, Rect::new(5, 5, 10, 10)),
        )
        .unwrap();
    let log = Rc::new(RefCell::new(Vec::new()));
    backend.set_event_sink(window, Rc::new(Recorder(Rc::clone(&log))));
    backend.set_visible(parent, false);

    let consumed = backend.inject(
        window,
        Event::MouseDown {
            x: 27,
            y: 37,
            button: MouseButton::Left,
            modifiers: Modifiers::NONE,
        },
    );

    assert!(!consumed, "no node is hit");
    assert!(
        log.borrow().is_empty(),
        "the hidden subtree receives nothing"
    );
}

#[test]
fn a_resized_move_notifies_the_node_but_a_reposition_does_not() {
    // A node has no HWND of its own to fire a native size message on, so a
    // parent that resizes it through `apply_moves` (as `Control::set_bounds`
    // does) must be told directly: otherwise a widget with layout cached from
    // its last `Event::Resize` (a scrollbar's track, say) never sees the new
    // size.
    let backend = OffscreenBackend::new();
    let window = backend.open_window(&PlatformSpec::new("resize")).unwrap();
    let node = backend
        .create(
            ParentRef::Window(window),
            &NodeSpec::new(NodeKind::Container, Rect::new(0, 0, 40, 20)),
        )
        .unwrap();
    let log = Rc::new(RefCell::new(Vec::new()));
    backend.set_event_sink(window, Rc::new(Recorder(Rc::clone(&log))));

    backend.apply_moves(window, &[(node, Rect::new(10, 10, 50, 30))]);
    assert!(
        log.borrow().is_empty(),
        "a same-size move is just a reposition, not a resize"
    );

    backend.apply_moves(window, &[(node, Rect::new(10, 10, 90, 30))]);
    assert!(
        matches!(
            log.borrow().last(),
            Some((target, Event::Resize { width: 80, height: 20 })) if *target == node
        ),
        "a size change delivers Event::Resize to the moved node itself"
    );
}

#[test]
fn raising_a_node_puts_it_above_an_overlapping_sibling() {
    let backend = OffscreenBackend::new();
    let window = backend.open_window(&PlatformSpec::new("raise")).unwrap();
    let make = || {
        backend
            .create(
                ParentRef::Window(window),
                &NodeSpec::new(NodeKind::Container, Rect::new(0, 0, 50, 50)),
            )
            .unwrap()
    };
    let (below, above) = (make(), make());
    let log = Rc::new(RefCell::new(Vec::new()));
    backend.set_event_sink(window, Rc::new(Recorder(Rc::clone(&log))));
    let click = || Event::MouseDown {
        x: 10,
        y: 10,
        button: MouseButton::Left,
        modifiers: Modifiers::NONE,
    };

    backend.inject(window, click());
    assert_eq!(log.borrow().last().map(|(id, _)| *id), Some(above));

    backend.raise(below);
    backend.inject(window, click());
    assert_eq!(
        log.borrow().last().map(|(id, _)| *id),
        Some(below),
        "the raised node is now on top"
    );
}
